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
/// Read off the first block with audio, which is where a hybrid or DSD
/// stream shows itself. An unreadable or foreign file answers `None`: the
/// scanner's extractor is what reports those, and refusing here would turn
/// an unreadable file into an absent one.
pub fn refusal(path: &Path) -> Option<&'static str> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; HEADER_LEN];
    loop {
        file.read_exact(&mut header).ok()?;
        match parse_header(&header) {
            Ok(parsed) if parsed.block_samples == 0 => {
                let body = parsed.block_len().saturating_sub(HEADER_LEN) as i64;
                file.seek(SeekFrom::Current(body)).ok()?;
            }
            Ok(_) => return None,
            Err(WavpackError::OutOfScope(Scope::Hybrid)) => return Some("hybrid WavPack"),
            Err(WavpackError::OutOfScope(Scope::Dsd)) => return Some("WavPack DSD"),
            Err(WavpackError::UnsupportedVersion(_)) => return Some("pre-4.0 WavPack"),
            Err(_) => return None,
        }
    }
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
    total_frames: Option<u64>,
    /// `(first frame, frames, file offset)` of every packet found so far, in
    /// order. Filled up to the seek target and no further, since finding it
    /// walks the block headers.
    index: Vec<(u64, u32, u64)>,
    /// Where the walk continues, or `None` once it reached the end.
    index_next: Option<u64>,
    /// Frames of the next packet that lie before a seek's target.
    trim: u64,
}

impl<'s> WavPackReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let start = mss.pos();
        let first = loop {
            match read_block(&mut mss)? {
                Some(block) if block.header.block_samples == 0 => continue,
                Some(block) => break block,
                None => return decode_error("wavpack: stream holds no audio"),
            }
        };
        let flags = first.header.flags;
        let layout = stream_layout(&first)?;

        let bits = (flags.bytes_per_sample() * 8).saturating_sub(flags.output_shift());
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_WAVPACK)
            .with_sample_rate(layout.sample_rate)
            .with_channels(layout.channels)
            .with_sample_format(sample_format(flags)?)
            .with_bits_per_sample(bits)
            .with_max_frames_per_packet(u64::from(first.header.block_samples));

        let total_frames = first.header.total_samples;
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
            index: Vec::new(),
            index_next: Some(start),
            trim: 0,
        })
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
        if self.total_frames.is_some_and(|total| target > total) {
            return seek_error(SeekErrorKind::OutOfRange);
        }
        let Some((first, offset)) = self.locate(target)? else {
            return seek_error(SeekErrorKind::OutOfRange);
        };
        self.reader.seek(SeekFrom::Start(offset))?;
        self.pending = None;
        self.trim = target - first;
        Ok(SeekedTo {
            track_id: 0,
            required_ts,
            actual_ts: required_ts,
        })
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        let Some(first) = self.next_audio_block()? else {
            return Ok(None);
        };
        let first_frame = first.header.block_index;
        let frames = u64::from(first.header.block_samples);
        let mut is_final = first.header.flags.final_block();
        let mut data = first.data;
        while !is_final {
            let Some(block) = self.next_audio_block()? else {
                return decode_error("wavpack: file ends inside a multichannel packet");
            };
            is_final = block.header.flags.final_block();
            data.extend_from_slice(&block.data);
        }
        let trim = std::mem::take(&mut self.trim).min(frames);
        let mut packet = Packet::new(
            0,
            Timestamp::new((first_frame + trim) as i64),
            Duration::from(frames - trim),
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
                GenericAudioBuffer::S8(buf) => store(buf, samples, width, filled, |v| v as i8),
                GenericAudioBuffer::S16(buf) => store(buf, samples, width, filled, |v| v as i16),
                GenericAudioBuffer::S24(buf) => store(buf, samples, width, filled, i24),
                GenericAudioBuffer::S32(buf) => store(buf, samples, width, filled, |v| v),
                GenericAudioBuffer::F32(buf) => {
                    store(buf, samples, width, filled, |v| f32::from_bits(v as u32))
                }
                _ => decode_error("wavpack: unexpected sample format"),
            }?;
            filled += width;
        }
        if filled != self.channels {
            return decode_error("wavpack: packet does not match the stream's layout");
        }

        // Regardless of the gapless option: this is where a seek landed, not
        // encoder priming, and a lossless stream has none.
        let trim = (packet.trim_start.get() as usize).min(frames.unwrap_or(0));
        self.buf.trim(trim, 0);
        Ok(())
    }
}

/// Write one block's interleaved samples into the planes from channel
/// `first` on, sizing the buffer on the packet's first block.
fn store<S: Sample>(
    buf: &mut AudioBuffer<S>,
    samples: &[i32],
    width: usize,
    first: usize,
    convert: impl Fn(i32) -> S,
) -> Result<()> {
    let frames = samples.len() / width.max(1);
    if first == 0 {
        buf.grow_capacity(frames);
        buf.render_uninit(Some(frames));
    }
    for channel in 0..width {
        let Some(plane) = buf.plane_mut(first + channel) else {
            return decode_error("wavpack: missing audio plane");
        };
        for (frame, sample) in plane.iter_mut().enumerate().take(frames) {
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
