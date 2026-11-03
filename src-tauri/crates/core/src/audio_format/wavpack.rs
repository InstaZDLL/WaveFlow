//! WavPack (`.wv`), as a symphonia reader and decoder over `wavicle`.
//!
//! ## Why `wavicle`
//!
//! Measured bit for bit against `wvunpack` 5.9.0 on 84 files (8 to 32 bits,
//! integer and float, mono to 7.1, six compression settings).
//! `symphonia-codec-wavpack`, the obvious candidate, got 38 of them right: 40
//! stopped on a false end of stream, every stereo file above the default
//! setting among them, and on the other 6, all 32-bit, it reported success
//! with the low 8 bits of most samples wrong.
//! `wavicle` matched all 60 files in its stated scope and refused the other
//! 24 by name, and it checks every block's CRC and turns a mismatch into an
//! error where the reference mutes the block.
//!
//! ## What it does not play
//!
//! Hybrid files (lossy on their own, lossless only with a `.wvc` beside
//! them) and DSD are out of `wavicle`'s scope. Both are refused when the
//! stream is opened, so [`refusal`] keeps them out of the library; a hybrid
//! file never gets the chance to pass for a lossless one.
//!
//! Two limits of the crate are lifted here without touching its decoding,
//! since neither has any bearing on it: see [`decodable_block`].

use std::io::{ErrorKind, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;

use symphonia::core::audio::layouts::{CHANNEL_LAYOUT_MONO, CHANNEL_LAYOUT_STEREO};
use symphonia::core::audio::sample::{i24, Sample, SampleFormat};
use symphonia::core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBuffer,
    GenericAudioBufferRef, Position,
};
use symphonia::core::codecs::audio::well_known::CODEC_ID_WAVPACK;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia::core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia::core::codecs::{CodecInfo, CodecParameters};
use symphonia::core::errors::{
    decode_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
use symphonia::core::formats::probe::{
    ProbeDataMatchSpec, ProbeFormatData, ProbeableFormat, Score, Scoreable,
};
use symphonia::core::formats::well_known::FORMAT_ID_WAVPACK;
use symphonia::core::formats::{
    FormatInfo, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia::core::meta::{ChapterGroup, Metadata, MetadataLog};
use symphonia::core::packet::{Packet, PacketRef};
use symphonia::core::units::{Duration, Timestamp};
use wavicle::format::{meta, Flags};
use wavicle::{BlockHeader, Error as WavpackError, Scope};

const HEADER_LEN: usize = 32;
const MARKER: &[u8; 4] = b"wvpk";
const MAX_CHANNELS: u16 = 32;

/// A seek that lands with less of its block left than this takes the next
/// blocks along in the same packet: the end of a block heard alone, then
/// silence while the next one decodes, sounds like a click.
const LANDING_TAIL_MS: u64 = 250;

const FORMAT_INFO: FormatInfo = FormatInfo {
    format: FORMAT_ID_WAVPACK,
    short_name: "wavpack",
    long_name: "WavPack",
};

/// Parse a block header, with one check in front of the crate's.
///
/// `wavicle` adds 8 to the stored block size in 32 bits before comparing it
/// with its limit. A size in the last few values of the range wraps in a
/// release build and passes, asking for a block of 4 GiB, and panics in a
/// debug one. Bytes that merely spell the marker can say anything.
fn parse_header(head: &[u8]) -> std::result::Result<BlockHeader, WavpackError> {
    if let Some(size) = head.get(4..8) {
        let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]);
        if u64::from(size) + 8 > u64::from(wavicle::format::MAX_BLOCK_SIZE) {
            return Err(WavpackError::BadBlockSize(size));
        }
    }
    BlockHeader::parse(head)
}

/// What the crate's refusal of a header means for the reader.
///
/// Spelled out because symphonia's error takes a static string: an
/// out-of-scope stream has to say which scope, or a log reader has no way to
/// tell a hybrid file from a broken one.
fn header_error(err: WavpackError) -> Error {
    match err {
        WavpackError::OutOfScope(Scope::Hybrid) => {
            Error::Unsupported("wavpack: hybrid (lossy) streams are not supported")
        }
        WavpackError::OutOfScope(Scope::Dsd) => {
            Error::Unsupported("wavpack: DSD streams are not supported")
        }
        WavpackError::UnsupportedVersion(_) => {
            Error::Unsupported("wavpack: stream version is not supported")
        }
        _ => Error::DecodeError("wavpack: invalid block header"),
    }
}

/// Why this build would not play the WavPack file at `path`, or `None` when
/// it would.
///
/// Answered by the reader itself, from the first block with audio, which is
/// where a hybrid or DSD stream shows itself: what it refuses here it refuses
/// at play time. A file that is not WavPack at all is refused too. lofty takes
/// the extension's word for it and reports no error, so left to the extractor
/// it would become a silent row. Only a file that cannot be opened answers
/// `None`, for the extractor to report.
pub fn refusal(path: &Path) -> Option<&'static str> {
    let mut file = std::fs::File::open(path).ok()?;
    if skip_id3v2(&mut file).is_err() {
        return Some("not a WavPack stream");
    }
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    // Not looking at the end of the file: this runs on every file of every
    // scan, and a file cut short plays what it holds.
    match WavPackReader::open(mss, FormatOptions::default(), false) {
        Ok(_) => None,
        Err(Error::Unsupported(reason)) => Some(reason),
        Err(_) => Some("not a WavPack stream"),
    }
}

/// How long the WavPack file at `path` really plays, when it holds less than
/// its first block announces. `None` for a whole file, or an unreadable one.
pub fn held_duration_ms(path: &Path) -> Option<i64> {
    let mut file = std::fs::File::open(path).ok()?;
    skip_id3v2(&mut file).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let reader = WavPackReader::open(mss, FormatOptions::default(), true).ok()?;
    let held = reader.total_frames?;
    if reader
        .declared_frames
        .is_some_and(|declared| held >= declared)
    {
        return None;
    }
    let rate = reader.tracks[0]
        .codec_params
        .as_ref()?
        .audio()?
        .sample_rate?;
    Some((held * 1000 / u64::from(rate)) as i64)
}

/// How far the audio in the file really goes, from the last block in its tail.
///
/// The first block states the length of the file as it was written, so one cut
/// short still announces all of it. A block is at most 1 MiB, so its last one
/// starts within this much of the end unless a large tag follows it, and then
/// nothing is found and the announced length stands. A block the file does not
/// hold all of, or the first half of a multichannel packet, does not play, so
/// the audio ends where it starts. `None` when that cannot be told.
fn frames_held<R: std::io::Read + Seek>(source: &mut R, declared: u64) -> Option<u64> {
    use std::io::Read;

    let len = source.seek(SeekFrom::End(0)).ok()?;
    let tail_len = len.min(2 * u64::from(wavicle::format::MAX_BLOCK_SIZE));
    source.seek(SeekFrom::Start(len - tail_len)).ok()?;
    let mut tail = Vec::with_capacity(tail_len as usize);
    source.take(tail_len).read_to_end(&mut tail).ok()?;

    let mut before = tail.len();
    while let Some(at) = tail[..before].windows(4).rposition(|w| w == MARKER) {
        before = at;
        let Some(Ok(header)) = tail.get(at..at + HEADER_LEN).map(parse_header) else {
            continue;
        };
        let ends_at = header.block_index + u64::from(header.block_samples);
        if header.block_samples == 0 || ends_at > declared {
            continue;
        }
        let whole = at + header.block_len() <= tail.len() && header.flags.final_block();
        return Some(if whole { ends_at } else { header.block_index });
    }
    None
}

/// Leave `file` at the end of the ID3v2 tag it starts with, if it has one.
/// Taggers put one there though the format has no place for it; the probe
/// reads past it the same way.
fn skip_id3v2(file: &mut std::fs::File) -> std::io::Result<()> {
    use std::io::Read;

    let mut head = [0u8; 10];
    file.read_exact(&mut head)?;
    let at = if &head[..3] == b"ID3" {
        // Seven bits to the byte, and the ten bytes of the header itself.
        head[6..10]
            .iter()
            .fold(0u64, |size, byte| (size << 7) | u64::from(byte & 0x7f))
            + 10
    } else {
        0
    };
    file.seek(SeekFrom::Start(at))?;
    Ok(())
}

/// One block as read from the stream: its parsed header and all its bytes.
struct RawBlock {
    header: BlockHeader,
    data: Vec<u8>,
}

/// Read the block at the stream's position. `None` at the end of the audio:
/// the end of the file, or the tag that follows the last block.
fn read_block(reader: &mut MediaSourceStream<'_>) -> Result<Option<RawBlock>> {
    let mut head = [0u8; HEADER_LEN];
    match reader.read_buf_exact(&mut head) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }
    if &head[..4] != MARKER {
        return Ok(None);
    }
    let header = parse_header(&head).map_err(header_error)?;
    let mut data = vec![0u8; header.block_len()];
    data[..HEADER_LEN].copy_from_slice(&head);
    match reader.read_buf_exact(&mut data[HEADER_LEN..]) {
        Ok(()) => Ok(Some(RawBlock { header, data })),
        // A file cut short inside a block: what came before it still plays.
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
            tracing::warn!("wavpack: file ends inside a block; ending the track there");
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

/// The layout a stream declares, from the first block with audio.
struct StreamLayout {
    sample_rate: u32,
    channels: Channels,
}

fn stream_layout(block: &RawBlock) -> Result<StreamLayout> {
    let flags = block.header.flags;
    let mut sample_rate = flags.sample_rate();
    let mut declared: Option<(u16, u32)> = None;
    let mut at = HEADER_LEN;
    while at + 2 <= block.data.len() {
        let (id, body, next) = sub_block_at(&block.data, at)?;
        let body = &block.data[body];
        match id {
            meta::SAMPLE_RATE if body.len() >= 3 => {
                sample_rate = Some(u32::from_le_bytes([body[0], body[1], body[2], 0]));
            }
            meta::CHANNEL_INFO => declared = channel_info(body),
            _ => {}
        }
        at = next;
    }
    let Some(sample_rate) = sample_rate else {
        return decode_error("wavpack: stream does not declare a sample rate");
    };
    let channels = match declared {
        // More than any layout goes to, so only a damaged block says it. The
        // buffer is sized from this before a single sample is decoded.
        Some((count, _)) if count > MAX_CHANNELS => {
            return unsupported_error("wavpack: implausible channel count");
        }
        Some((count, mask)) if count > 2 => layout_from_mask(count, mask),
        _ if flags.output_channels() == 1 => CHANNEL_LAYOUT_MONO,
        _ => CHANNEL_LAYOUT_STEREO,
    };
    Ok(StreamLayout {
        sample_rate,
        channels,
    })
}

/// Channel count and WAVE mask from an `ID_CHANNEL_INFO` body, per the
/// reference's `read_channel_info`: one layout for the 6- and 7-byte form,
/// whose count runs to 4096 channels, and another for everything shorter.
fn channel_info(body: &[u8]) -> Option<(u16, u32)> {
    let le_mask = |bytes: &[u8]| {
        bytes
            .iter()
            .enumerate()
            .fold(0u32, |mask, (i, byte)| mask | u32::from(*byte) << (8 * i))
    };
    match body.len() {
        6 | 7 => {
            let count = (u16::from(body[0]) | u16::from(body[2] & 0x0f) << 8) + 1;
            Some((count, le_mask(&body[3..])))
        }
        1..=5 => Some((u16::from(body[0]), le_mask(&body[1..]))),
        _ => None,
    }
}

/// Positions when the mask names one per channel, discrete channels when it
/// does not: a wrong position sends a channel to the wrong speaker, and
/// discrete says only what is known.
fn layout_from_mask(count: u16, mask: u32) -> Channels {
    match Position::from_wave_channel_mask(mask) {
        Some(position) if mask.count_ones() == u32::from(count) => Channels::Positioned(position),
        _ => Channels::Discrete(count),
    }
}

/// Function id, body range and the offset of the next sub-block, for the
/// metadata sub-block whose header is at `at`.
fn sub_block_at(block: &[u8], at: usize) -> Result<(u8, Range<usize>, usize)> {
    let raw_id = block[at];
    let (words, body_at) = if raw_id & meta::LARGE != 0 {
        let Some(size) = block.get(at + 1..at + 4) else {
            return decode_error("wavpack: truncated sub-block header");
        };
        let words = usize::from(size[0]) | usize::from(size[1]) << 8 | usize::from(size[2]) << 16;
        (words, at + 4)
    } else {
        (usize::from(block[at + 1]), at + 2)
    };
    let next = body_at + words * 2;
    if next > block.len() {
        return decode_error("wavpack: sub-block overruns its block");
    }
    let odd = usize::from(raw_id & meta::ODD_SIZE != 0);
    if odd > words * 2 {
        return decode_error("wavpack: odd-sized sub-block with no body");
    }
    Ok((raw_id & meta::ID_MASK, body_at..next - odd, next))
}

/// The stored sample width in a block's flags, as bytes minus one.
const BYTES_STORED_MASK: u32 = 0b11;
/// Offset of the flags in a block.
const FLAGS_AT: usize = 24;

/// The bytes to hand `wavicle` for this block, which are the block itself
/// unless one of two refusals would apply that have nothing to do with how
/// the samples decode.
///
/// A multichannel stream is a run of independent mono and stereo blocks,
/// which `wavicle` decodes as readily as any other; it refuses them only on
/// seeing a channel count above two, so that sub-block is renamed to the
/// dummy id every reader skips. An 8-bit stream is refused outright, though
/// the stored width takes no part in decoding (the block's CRC runs over the
/// samples, not their width), so the copy declares 16 bits. The samples come
/// back at their own scale either way: measured against `wvunpack`, 5.1, 7.1
/// and 8-bit all match bit for bit.
fn decodable_block<'a>(block: &'a [u8], scratch: &'a mut Vec<u8>) -> Result<&'a [u8]> {
    let flags = u32::from_le_bytes([
        block[FLAGS_AT],
        block[FLAGS_AT + 1],
        block[FLAGS_AT + 2],
        block[FLAGS_AT + 3],
    ]);
    let is_8_bit = flags & BYTES_STORED_MASK == 0;

    let mut channel_info_at = None;
    let mut at = HEADER_LEN;
    while at + 2 <= block.len() {
        let (id, _, next) = sub_block_at(block, at)?;
        if id == meta::CHANNEL_INFO {
            channel_info_at = Some(at);
        }
        at = next;
    }
    if !is_8_bit && channel_info_at.is_none() {
        return Ok(block);
    }

    scratch.clear();
    scratch.extend_from_slice(block);
    if is_8_bit {
        scratch[FLAGS_AT] |= 1;
    }
    if let Some(at) = channel_info_at {
        // Only the function bits: the size flags still describe the body.
        scratch[at] = (scratch[at] & !meta::ID_MASK) | meta::DUMMY;
    }
    Ok(scratch)
}

/// Native `.wv` block reader. A packet is every block of one time index,
/// which is one for stereo and one per channel pair for multichannel.
pub struct WavPackReader<'s> {
    reader: MediaSourceStream<'s>,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    /// The first block with audio, read while opening.
    pending: Option<RawBlock>,
    /// The frames there are to play, which a file cut short has fewer of than
    /// its first block says.
    total_frames: Option<u64>,
    /// What the first block says.
    declared_frames: Option<u64>,
    /// `(first frame, frames, file offset)` of every packet found so far, in
    /// order. Filled up to the seek target and no further, since finding it
    /// walks the block headers.
    index: Vec<(u64, u32, u64)>,
    /// Where the walk continues, or `None` once it reached the end.
    index_next: Option<u64>,
    /// Frames of the next packet that lie before a seek's target.
    trim: u64,
    /// [`LANDING_TAIL_MS`] in frames.
    landing_tail: u64,
}

impl<'s> WavPackReader<'s> {
    pub fn try_new(mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        Self::open(mss, opts, true)
    }

    /// `check_end` also looks for where the audio really ends, which takes a
    /// read at the end of the file.
    fn open(mut mss: MediaSourceStream<'s>, opts: FormatOptions, check_end: bool) -> Result<Self> {
        let start = mss.pos();
        let first = loop {
            match read_block(&mut mss)? {
                Some(block) if block.header.block_samples == 0 => continue,
                Some(block) => break block,
                None => return decode_error("wavpack: stream holds no audio"),
            }
        };
        // A multichannel packet is several blocks, and one the file does not
        // hold all of does not play: with no whole packet at all, this would
        // open as a stream with nothing in it.
        if !first.header.flags.final_block() {
            let resume = mss.pos();
            loop {
                match read_block(&mut mss)? {
                    Some(block) if block.header.flags.final_block() => break,
                    Some(_) => {}
                    None => return decode_error("wavpack: file ends inside its first packet"),
                }
            }
            mss.seek(SeekFrom::Start(resume))?;
        }
        let flags = first.header.flags;
        let layout = stream_layout(&first)?;
        let landing_tail = u64::from(layout.sample_rate) * LANDING_TAIL_MS / 1000;

        let bits = (flags.bytes_per_sample() * 8).saturating_sub(flags.output_shift());
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_WAVPACK)
            .with_sample_rate(layout.sample_rate)
            .with_channels(layout.channels)
            .with_sample_format(sample_format(flags)?)
            .with_bits_per_sample(bits)
            .with_max_frames_per_packet(u64::from(first.header.block_samples));

        let declared_frames = first.header.total_samples;
        let mut total_frames = declared_frames;
        if let (true, true, Some(declared)) = (check_end, mss.is_seekable(), declared_frames) {
            let resume = mss.pos();
            let held = frames_held(&mut mss, declared);
            mss.seek(SeekFrom::Start(resume))?;
            if let Some(held) = held.filter(|held| *held < declared) {
                tracing::warn!(
                    frames_held = held,
                    frames_announced = declared,
                    "wavpack: the file is cut short; it ends with its last whole block"
                );
                total_frames = Some(held);
            }
        }
        let mut track = Track::new(0);
        track.with_codec_params(CodecParameters::Audio(params));
        if let Some(total) = total_frames {
            track
                .with_num_frames(total)
                .with_duration(Duration::from(total));
        }
        let media_info = MediaInfo::from_track(&track);

        Ok(Self {
            reader: mss,
            media_info,
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            pending: Some(first),
            total_frames,
            declared_frames,
            index: Vec::new(),
            index_next: Some(start),
            trim: 0,
            landing_tail,
        })
    }

    /// The blocks of one stretch of frames, one per channel or channel pair,
    /// as `(first frame, frames, their bytes)`.
    fn next_stretch(&mut self) -> Result<Option<(u64, u64, Vec<u8>)>> {
        let Some(first) = self.next_audio_block()? else {
            return Ok(None);
        };
        let first_frame = first.header.block_index;
        let frames = u64::from(first.header.block_samples);
        let mut is_final = first.header.flags.final_block();
        let mut data = first.data;
        while !is_final {
            let Some(block) = self.next_audio_block()? else {
                // As for a file cut short inside a block: it ends here.
                tracing::warn!("wavpack: file ends inside a packet; ending the track there");
                return Ok(None);
            };
            is_final = block.header.flags.final_block();
            data.extend_from_slice(&block.data);
        }
        Ok(Some((first_frame, frames, data)))
    }

    fn next_audio_block(&mut self) -> Result<Option<RawBlock>> {
        if let Some(block) = self.pending.take() {
            return Ok(Some(block));
        }
        while let Some(block) = read_block(&mut self.reader)? {
            if block.header.block_samples != 0 {
                return Ok(Some(block));
            }
        }
        Ok(None)
    }

    /// Walk the block headers until the packet holding `target` is indexed
    /// or the audio ends, and answer the last packet that starts at or
    /// before it, as `(first frame, file offset)`.
    fn locate(&mut self, target: u64) -> Result<Option<(u64, u64)>> {
        while let Some(offset) = self.index_next {
            if let Some((first, frames, _)) = self.index.last() {
                if first + u64::from(*frames) > target {
                    break;
                }
            }
            self.reader.seek(SeekFrom::Start(offset))?;
            let mut head = [0u8; HEADER_LEN];
            let header = match self.reader.read_buf_exact(&mut head) {
                Ok(()) if &head[..4] == MARKER => Some(parse_header(&head).map_err(header_error)?),
                Ok(()) => None,
                Err(err) if err.kind() == ErrorKind::UnexpectedEof => None,
                Err(err) => return Err(err.into()),
            };
            let Some(header) = header else {
                self.index_next = None;
                break;
            };
            if header.block_samples != 0 && header.flags.initial_block() {
                self.index
                    .push((header.block_index, header.block_samples, offset));
            }
            self.index_next = Some(offset + header.block_len() as u64);
        }
        let after = self.index.partition_point(|(first, _, _)| *first <= target);
        Ok(after
            .checked_sub(1)
            .map(|i| (self.index[i].0, self.index[i].2)))
    }
}

impl Scoreable for WavPackReader<'_> {
    /// Sure of anything shaped like a WavPack block, hybrid and DSD
    /// included: they are refused when the reader is built, with a reason.
    /// Scored as unsupported instead, the probe would walk on into the
    /// stream's bytes and might take some of them for an MP3 sync word,
    /// which plays as noise.
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut head = [0u8; HEADER_LEN];
        src.read_buf_exact(&mut head)?;
        Ok(match parse_header(&head) {
            Ok(_) | Err(WavpackError::OutOfScope(_)) | Err(WavpackError::UnsupportedVersion(_)) => {
                Score::Supported(255)
            }
            Err(_) => Score::Unsupported,
        })
    }
}

impl ProbeableFormat<'_> for WavPackReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(WavPackReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        // Spelled out rather than built with `support_format!`, for the
        // reason `opus.rs` gives about the codec macro.
        &[ProbeFormatData {
            spec: ProbeDataMatchSpec {
                extensions: &["wv"],
                mime_types: &["audio/wavpack", "audio/x-wavpack"],
                markers: &[MARKER],
            },
            info: FORMAT_INFO,
        }]
    }
}

impl FormatReader for WavPackReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        &FORMAT_INFO
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.chapters.as_ref()
    }

    /// Lands on the exact frame asked for, in either mode: the first packet
    /// after the seek carries the head of its block as `trim_start`, which
    /// the decoder drops. A block runs up to half a second, so without it
    /// every seek would play audio from before the point the user chose.
    fn seek(&mut self, _mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        if !self.reader.is_seekable() {
            return seek_error(SeekErrorKind::Unseekable);
        }
        let required_ts = match to {
            SeekTo::Timestamp { ts, .. } => ts,
            SeekTo::Time { time, .. } => {
                let base = self.tracks[0]
                    .time_base
                    .ok_or(Error::SeekError(SeekErrorKind::Unseekable))?;
                base.calc_timestamp(time)
                    .ok_or(Error::SeekError(SeekErrorKind::OutOfRange))?
            }
        };
        let Ok(target) = u64::try_from(required_ts.get()) else {
            return seek_error(SeekErrorKind::OutOfRange);
        };
        if self
            .declared_frames
            .is_some_and(|declared| target > declared)
        {
            return seek_error(SeekErrorKind::OutOfRange);
        }
        // Into what a file cut short no longer holds: the end of the track, and
        // the queue moves on, as at the end of any other.
        let target = self.total_frames.map_or(target, |total| target.min(total));
        // Finding the target moves the stream. A seek that fails has to leave
        // it where the next packet is, or the track ends here.
        let resume = self.reader.pos();
        let located = self.locate(target);
        if !matches!(located, Ok(Some(_))) {
            self.reader.seek(SeekFrom::Start(resume))?;
        }
        let Some((first, offset)) = located? else {
            return seek_error(SeekErrorKind::OutOfRange);
        };
        self.reader.seek(SeekFrom::Start(offset))?;
        self.pending = None;
        self.trim = target - first;
        Ok(SeekedTo {
            track_id: 0,
            required_ts,
            actual_ts: Timestamp::new(target as i64),
        })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let Some((first_frame, frames, mut data)) = self.next_stretch()? else {
            return Ok(None);
        };
        let trim = std::mem::take(&mut self.trim).min(frames);
        let mut held = frames - trim;
        if trim > 0 {
            while held < self.landing_tail {
                let Some((_, more, blocks)) = self.next_stretch()? else {
                    break;
                };
                held += more;
                data.extend_from_slice(&blocks);
            }
        }
        let mut packet = Packet::new(
            0,
            Timestamp::new((first_frame + trim) as i64),
            Duration::from(held),
            data,
        );
        packet.trim_start = Duration::from(trim);
        Ok(Some(packet))
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

fn sample_format(flags: Flags) -> Result<SampleFormat> {
    if flags.is_float() {
        return Ok(SampleFormat::F32);
    }
    match flags.bytes_per_sample() {
        1 => Ok(SampleFormat::S8),
        2 => Ok(SampleFormat::S16),
        3 => Ok(SampleFormat::S24),
        4 => Ok(SampleFormat::S32),
        _ => decode_error("wavpack: invalid sample width"),
    }
}

/// Decoder for the packets of [`WavPackReader`].
pub struct WavPackDecoder {
    params: AudioCodecParameters,
    buf: GenericAudioBuffer,
    channels: usize,
    scratch: Vec<u8>,
}

impl WavPackDecoder {
    fn try_new(params: &AudioCodecParameters) -> Result<Self> {
        let (Some(rate), Some(channels), Some(format)) = (
            params.sample_rate,
            params.channels.clone(),
            params.sample_format,
        ) else {
            return unsupported_error("wavpack: incomplete codec parameters");
        };
        let count = channels.count();
        let capacity = params.max_frames_per_packet.unwrap_or(0) as usize;
        Ok(Self {
            params: params.clone(),
            buf: GenericAudioBuffer::new(format, AudioSpec::new(rate, channels), capacity),
            channels: count,
            scratch: Vec::new(),
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();

        // A packet is one stretch of frames, or several after a seek
        // (`LANDING_TAIL_MS`). Each ends on a block marked final.
        let mut start = 0usize;
        let mut first_stretch = None;
        let mut filled = 0usize;
        let mut frames = None;
        let mut at = 0usize;
        while at < packet.data.len() {
            let Some(head) = packet.data.get(at..at + HEADER_LEN) else {
                return decode_error("wavpack: truncated packet");
            };
            let header = parse_header(head).map_err(header_error)?;
            let Some(block) = packet.data.get(at..at + header.block_len()) else {
                return decode_error("wavpack: block overruns its packet");
            };
            at += header.block_len();

            let block_frames = header.block_samples as usize;
            if *frames.get_or_insert(block_frames) != block_frames {
                return decode_error("wavpack: packet mixes block lengths");
            }
            let stream = decodable_block(block, &mut self.scratch)?;
            let decoded = wavicle::decode_stream(stream).map_err(|err| {
                tracing::warn!(error = %err, "wavpack: block refused");
                Error::DecodeError("wavpack: undecodable block")
            })?;
            let width = decoded.channels as usize;
            if decoded.samples.len() != block_frames * width || filled + width > self.channels {
                return decode_error("wavpack: packet does not match the stream's layout");
            }
            let samples = &decoded.samples;
            match &mut self.buf {
                GenericAudioBuffer::S8(buf) => {
                    store(buf, samples, width, start, filled, |v| v as i8)
                }
                GenericAudioBuffer::S16(buf) => {
                    store(buf, samples, width, start, filled, |v| v as i16)
                }
                GenericAudioBuffer::S24(buf) => store(buf, samples, width, start, filled, i24),
                GenericAudioBuffer::S32(buf) => store(buf, samples, width, start, filled, |v| v),
                GenericAudioBuffer::F32(buf) => store(buf, samples, width, start, filled, |v| {
                    f32::from_bits(v as u32)
                }),
                _ => decode_error("wavpack: unexpected sample format"),
            }?;
            filled += width;
            if header.flags.final_block() {
                if filled != self.channels {
                    return decode_error("wavpack: packet does not match the stream's layout");
                }
                first_stretch.get_or_insert(block_frames);
                start += block_frames;
                filled = 0;
                frames = None;
            }
        }
        if filled != 0 || start == 0 {
            return decode_error("wavpack: packet does not match the stream's layout");
        }

        // Regardless of the gapless option: this is where a seek landed, not
        // encoder priming, and a lossless stream has none.
        let trim = (packet.trim_start.get() as usize).min(first_stretch.unwrap_or(0));
        self.buf.trim(trim, 0);
        Ok(())
    }
}

/// Write one block's interleaved samples into the planes, from channel
/// `first` and from frame `start` on, making room on a stretch's first block.
fn store<S: Sample>(
    buf: &mut AudioBuffer<S>,
    samples: &[i32],
    width: usize,
    start: usize,
    first: usize,
    convert: impl Fn(i32) -> S,
) -> Result<()> {
    let frames = samples.len() / width.max(1);
    if first == 0 {
        buf.grow_capacity(start + frames);
        buf.render_uninit(Some(frames));
    }
    for channel in 0..width {
        let Some(plane) = buf.plane_mut(first + channel) else {
            return decode_error("wavpack: missing audio plane");
        };
        for (frame, sample) in plane[start..].iter_mut().enumerate().take(frames) {
            *sample = convert(samples[frame * width + channel]);
        }
    }
    Ok(())
}

impl AudioDecoder for WavPackDecoder {
    /// Nothing to forget: every block carries all its decoder needs.
    fn reset(&mut self) {}

    fn codec_info(&self) -> &CodecInfo {
        &Self::supported_codecs()
            .first()
            .expect("at least one codec registered")
            .info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        match self.decode_inner(packet) {
            Ok(()) => Ok(self.buf.as_generic_audio_buffer_ref()),
            Err(err) => {
                // The trait requires an empty buffer after a failure.
                self.buf.clear();
                Err(err)
            }
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for WavPackDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        _opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(WavPackDecoder::try_new(params)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[SupportedAudioCodec {
            id: CODEC_ID_WAVPACK,
            info: CodecInfo {
                short_name: "wavpack",
                long_name: "WavPack",
                profiles: &[],
            },
        }]
    }
}
