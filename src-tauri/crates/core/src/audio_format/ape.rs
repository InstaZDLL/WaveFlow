//! Monkey's Audio (`.ape`), as a symphonia reader and decoder over
//! `ape-decoder`.
//!
//! ## Decoding happens ahead of the packets
//!
//! A Monkey's Audio frame is decoded whole: `ape-decoder` has no way to stop
//! inside one, and frames are long, 294 912 samples at Extra High and
//! 1 179 648 at Insane, which is 27 seconds of audio at 44.1 kHz. Measured on
//! a desktop CPU, one Insane stereo frame takes 1 to 2 s to decode. The output
//! ring holds about a second of stereo at 48 kHz and half that at 96 kHz, so
//! decoding on the decoder thread when a frame comes due stops the sound: the
//! file decodes far faster than real time and still drops out at every frame.
//!
//! So [`ApeReader`] keeps a worker thread that decodes the frame after the
//! one being played, and cuts decoded frames into small packets of PCM. The
//! next frame is ready long before it is due, and the thread that fills the
//! ring only ever copies. [`ApeDecoder`] is what is left of a decoder after
//! that: it turns those bytes into planes. It stays registered as one so the
//! three places that open a stream see an ordinary reader and decoder pair.
//!
//! Every frame's CRC is checked by the crate, so a frame that decodes wrong
//! comes back as an error and not as noise.
//!
//! ## What it does not play
//!
//! Measured bit for bit against the reference `mac` 13.45 on 68 files, every
//! level from Fast to Insane: 8, 16 and 24 bits at every channel count and
//! 32-bit mono all match. Two families do not, and are refused when the
//! stream is opened so [`refusal`] keeps them out of the library:
//!
//! - **32 bits with more than one channel.** Stereo overflows the crate's
//!   first-order filter, whose output the reference keeps in 64 bits, so
//!   frames fail their CRC from the first loud passage on. A file whose
//!   early frames happen to pass would start playing and then stop.
//! - **Floating point.** The samples are stored through a bit transform the
//!   crate leaves to its caller, and its CRC runs before that transform, so
//!   it would report success on what is noise.
//!
//! Files older than format 3950 (2002) are outside the crate as well.

use std::io::{ErrorKind, Seek, SeekFrom};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use ape_decoder::format::{self, ApeFileInfo, APE_FORMAT_FLAG_FLOATING_POINT};
use ape_decoder::{ApeError, FrameDecoder};
use symphonia::core::audio::layouts::{CHANNEL_LAYOUT_MONO, CHANNEL_LAYOUT_STEREO};
use symphonia::core::audio::sample::{i24, Sample, SampleFormat};
use symphonia::core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBuffer,
    GenericAudioBufferRef,
};
use symphonia::core::codecs::audio::well_known::CODEC_ID_MONKEYS_AUDIO;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia::core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia::core::codecs::{CodecInfo, CodecParameters};
use symphonia::core::common::FourCc;
use symphonia::core::errors::{
    decode_error, seek_error, unsupported_error, Error, Result, SeekErrorKind,
};
use symphonia::core::formats::probe::{
    ProbeDataMatchSpec, ProbeFormatData, ProbeableFormat, Score, Scoreable,
};
use symphonia::core::formats::{
    FormatId, FormatInfo, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia::core::meta::{ChapterGroup, Metadata, MetadataLog};
use symphonia::core::packet::{Packet, PacketRef};
use symphonia::core::units::{Duration, Timestamp};

/// `MACF` is the same format holding floating-point samples.
const MARKERS: &[&[u8]] = &[b"MAC ", b"MACF"];

/// The oldest file format the crate decodes.
const MIN_VERSION: u16 = 3950;

/// The range of version numbers the format has used and might yet, from
/// its first releases on. A number outside it is not a Monkey's Audio file
/// but bytes that happen to spell the marker.
const PLAUSIBLE_VERSIONS: std::ops::RangeInclusive<u16> = 3000..=9999;
/// From this version on a descriptor follows the marker, whose size the
/// score can check; before it, the compression level comes next.
const DESCRIPTOR_VERSION: u16 = 3980;

/// Frames in one PCM packet: the size symphonia's FLAC reader hands out for
/// CD audio, small enough that a packet never makes the output wait.
const PACKET_FRAMES: usize = 4096;

/// A frame compresses to about half its PCM. Twice the PCM, then, is far more
/// than any frame's compressed size, and it is what stops the read of the
/// last frame running through the tag that follows it: the seek table gives
/// that frame everything up to the end of the file.
const MAX_COMPRESSED_RATIO: u64 = 2;

const FORMAT_INFO: FormatInfo = FormatInfo {
    format: FormatId::new(FourCc::new(*b"mac ")),
    short_name: "ape",
    long_name: "Monkey's Audio",
};

/// What keeps a parsed file from playing, or `None` when it would.
fn unplayable(info: &ApeFileInfo) -> Option<&'static str> {
    if info.descriptor.version < MIN_VERSION {
        return Some("Monkey's Audio older than format 3950");
    }
    if info.header.format_flags & APE_FORMAT_FLAG_FLOATING_POINT != 0 {
        return Some("floating-point Monkey's Audio");
    }
    if info.header.bits_per_sample == 32 && info.header.channels != 1 {
        return Some("32-bit multichannel Monkey's Audio");
    }
    None
}

/// Why this build would not play the Monkey's Audio file at `path`, or `None`
/// when it would.
///
/// An unreadable or foreign file answers `None`: the scanner's extractor is
/// what reports those, and refusing here would turn an unreadable file into
/// an absent one.
pub fn refusal(path: &Path) -> Option<&'static str> {
    let mut file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    unplayable(&format::parse(&mut file).ok()?)
}

/// A frame on its way to the worker.
struct FrameJob {
    frame: u32,
    data: Vec<u8>,
    remainder: u32,
    blocks: usize,
}

/// What the worker answers for a frame: its PCM, or why it has none.
type FrameResult = (u32, std::result::Result<Vec<u8>, String>);

/// The thread that decodes ahead of the packets.
///
/// Dropping it ends the thread after the frame it is on. A seek relies on
/// that: it abandons the worker instead of waiting out a frame nobody wants
/// any more, up to a second of the listener's time at Insane.
struct Worker {
    jobs: Sender<FrameJob>,
    /// In a mutex for symphonia's `FormatReader: Sync` bound alone; only ever
    /// reached through `get_mut`, which takes no lock.
    results: Mutex<Receiver<FrameResult>>,
}

impl Worker {
    fn spawn(decoder: FrameDecoder) -> Result<Self> {
        let (jobs, job_queue) = channel::<FrameJob>();
        let (results, result_queue) = channel::<FrameResult>();
        std::thread::Builder::new()
            .name("ape-decode".into())
            .spawn(move || {
                let mut decoder = decoder;
                while let Ok(job) = job_queue.recv() {
                    let pcm = decoder
                        .decode_frame(&job.data, job.remainder, job.blocks)
                        .map_err(|err: ApeError| format!("{err:?}"));
                    if results.send((job.frame, pcm)).is_err() {
                        break;
                    }
                }
            })
            .map_err(Error::IoError)?;
        Ok(Self {
            jobs,
            results: Mutex::new(result_queue),
        })
    }
}

/// The frame being cut into packets.
struct CurrentFrame {
    index: u32,
    pcm: Vec<u8>,
    /// Bytes of `pcm` already handed out, or skipped by a seek.
    cursor: usize,
}

pub struct ApeReader<'s> {
    reader: MediaSourceStream<'s>,
    info: ApeFileInfo,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    metadata: MetadataLog,
    chapters: Option<ChapterGroup>,
    worker: Option<Worker>,
    /// The frame the worker was last given and has not answered for.
    in_flight: Option<u32>,
    current: Option<CurrentFrame>,
    /// The frame to play after `current`.
    next_frame: u32,
    /// Bytes of `next_frame` a seek landed past.
    next_skip: usize,
    /// Frame position of the next packet.
    position: u64,
}

impl<'s> ApeReader<'s> {
    pub fn try_new(mut mss: MediaSourceStream<'s>, opts: FormatOptions) -> Result<Self> {
        let info = format::parse(&mut mss).map_err(|err| match err {
            ApeError::Io(io) => Error::IoError(io),
            _ => Error::DecodeError("ape: invalid file header"),
        })?;
        if let Some(reason) = unplayable(&info) {
            tracing::debug!(reason, "ape: refusing the stream");
            return unsupported_error("ape: this stream is not supported");
        }
        let header = &info.header;
        let frames = header.total_frames as usize;
        if frames == 0
            || header.blocks_per_frame == 0
            || header.sample_rate == 0
            || header.channels == 0
            || info.seek_table.len() < frames
            || info.seek_table[..frames].windows(2).any(|w| w[0] > w[1])
        {
            return decode_error("ape: invalid stream layout");
        }

        let total = info.total_blocks as u64;
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_MONKEYS_AUDIO)
            .with_sample_rate(header.sample_rate)
            .with_channels(channel_layout(header.channels))
            .with_sample_format(sample_format(header.bits_per_sample)?)
            .with_bits_per_sample(u32::from(header.bits_per_sample))
            .with_max_frames_per_packet(PACKET_FRAMES as u64);
        let mut track = Track::new(0);
        track
            .with_codec_params(CodecParameters::Audio(params))
            .with_num_frames(total)
            .with_duration(Duration::from(total));
        let media_info = MediaInfo::from_track(&track);

        Ok(Self {
            reader: mss,
            info,
            media_info,
            tracks: vec![track],
            metadata: opts.external_data.metadata.unwrap_or_default(),
            chapters: opts.external_data.chapters,
            worker: None,
            in_flight: None,
            current: None,
            next_frame: 0,
            next_skip: 0,
            position: 0,
        })
    }

    fn block_align(&self) -> usize {
        usize::from(self.info.block_align)
    }

    fn total_frames(&self) -> u32 {
        self.info.header.total_frames
    }

    /// The compressed bytes of `frame`, read from the stream and addressed
    /// the way the crate's bit reader expects: from the 32-bit word the frame
    /// starts in, with a few bytes of slack after it.
    fn read_job(&mut self, frame: u32) -> Result<FrameJob> {
        let info = &self.info;
        let blocks = info.frame_block_count(frame) as usize;
        let start = info.seek_byte(frame);
        let remainder = ((start - info.seek_byte(0)) % 4) as u32;
        let pcm_size = blocks as u64 * u64::from(info.block_align);
        let compressed = info
            .frame_byte_count(frame)
            .min(pcm_size * MAX_COMPRESSED_RATIO + 4096);
        let want = compressed as usize + remainder as usize + 4;

        self.reader
            .seek(SeekFrom::Start(start - u64::from(remainder)))?;
        let mut data = vec![0u8; want];
        let mut filled = 0;
        while filled < want {
            match std::io::Read::read(&mut self.reader, &mut data[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) => return Err(err.into()),
            }
        }
        data.truncate(filled);
        Ok(FrameJob {
            frame,
            data,
            remainder,
            blocks,
        })
    }

    /// Hand the worker `frame` unless it is past the end or already there.
    fn request(&mut self, frame: u32) -> Result<()> {
        if frame >= self.total_frames() || self.in_flight == Some(frame) {
            return Ok(());
        }
        if self.worker.is_none() {
            let decoder = FrameDecoder::new(
                self.info.descriptor.version,
                self.info.header.channels,
                self.info.header.bits_per_sample,
                self.info.header.compression_level,
            )
            .map_err(|_| Error::Unsupported("ape: unsupported stream parameters"))?;
            self.worker = Some(Worker::spawn(decoder)?);
        }
        let job = self.read_job(frame)?;
        let Some(worker) = &self.worker else {
            return decode_error("ape: decode worker is gone");
        };
        if worker.jobs.send(job).is_err() {
            return decode_error("ape: decode worker stopped");
        }
        self.in_flight = Some(frame);
        Ok(())
    }

    /// Wait for `next_frame` and queue the one after it, which is what keeps
    /// a frame boundary from ever being a wait while the track plays.
    fn advance(&mut self) -> Result<std::result::Result<(), Packet>> {
        let frame = self.next_frame;
        self.request(frame)?;
        let Some(worker) = &mut self.worker else {
            return decode_error("ape: decode worker is gone");
        };
        let results = worker
            .results
            .get_mut()
            .map_err(|_| Error::DecodeError("ape: decode worker panicked"))?;
        let (answered, pcm) = results
            .recv()
            .map_err(|_| Error::DecodeError("ape: decode worker stopped"))?;
        if answered != frame {
            return decode_error("ape: decode worker answered out of order");
        }
        self.in_flight = None;
        self.next_frame = frame + 1;
        let skip = std::mem::take(&mut self.next_skip);

        let blocks = self.info.frame_block_count(frame);
        let frame_start = u64::from(frame) * u64::from(self.info.header.blocks_per_frame);
        self.position = frame_start + (skip / self.block_align()) as u64;

        let outcome = match pcm {
            Ok(pcm) if pcm.len() == blocks as usize * self.block_align() => {
                self.current = Some(CurrentFrame {
                    index: frame,
                    pcm,
                    cursor: skip,
                });
                Ok(())
            }
            other => {
                let reason = other.err().unwrap_or_else(|| "short frame".to_string());
                tracing::warn!(frame, reason, "ape: frame refused");
                // An empty packet, which the decoder reports as a decode
                // error: the caller's way of skipping a bad packet is the
                // one that applies. An error from here would end the track.
                self.current = None;
                let skipped = u64::from(blocks).saturating_sub(self.position - frame_start);
                let packet = Packet::new(
                    0,
                    Timestamp::new(self.position as i64),
                    Duration::from(skipped),
                    Vec::new(),
                );
                self.position += skipped;
                Err(packet)
            }
        };
        self.request(self.next_frame)?;
        Ok(outcome)
    }

    /// Drop what decoding is under way, for a seek somewhere else.
    fn abandon_worker(&mut self) {
        self.worker = None;
        self.in_flight = None;
    }
}

impl Scoreable for ApeReader<'_> {
    /// Sure of anything shaped like a Monkey's Audio header, the formats
    /// refused later included. Scored as unsupported instead, the probe would
    /// walk on into the stream's bytes and might take some of them for an MP3
    /// sync word, which plays as noise.
    ///
    /// Strict about the shape, because being sure also means the probe stops
    /// here if the reader then fails: four bytes spelling the marker in the
    /// junk ahead of another format's audio must not take that file down.
    fn score(mut src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        let mut head = [0u8; 12];
        src.read_buf_exact(&mut head)?;
        let version = u16::from_le_bytes([head[4], head[5]]);
        let shaped_right = if version >= DESCRIPTOR_VERSION {
            let descriptor = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
            (52..=1024).contains(&descriptor)
        } else {
            let level = u16::from_le_bytes([head[6], head[7]]);
            level % 1000 == 0 && (1000..=5000).contains(&level)
        };
        Ok(if PLAUSIBLE_VERSIONS.contains(&version) && shaped_right {
            Score::Supported(255)
        } else {
            Score::Unsupported
        })
    }
}

impl ProbeableFormat<'_> for ApeReader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        opts: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        Ok(Box::new(ApeReader::try_new(mss, opts)?))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        // Spelled out rather than built with `support_format!`, for the
        // reason `opus.rs` gives about the codec macro.
        &[ProbeFormatData {
            spec: ProbeDataMatchSpec {
                extensions: &["ape"],
                mime_types: &["audio/ape", "audio/x-ape", "audio/x-monkeys-audio"],
                markers: MARKERS,
            },
            info: FORMAT_INFO,
        }]
    }
}

impl FormatReader for ApeReader<'_> {
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

    /// Lands on the exact frame asked for, in either mode.
    ///
    /// Inside the frame already decoded that is a change of cursor and costs
    /// nothing. Anywhere else it is the wait for one frame to decode, however
    /// far into the frame the target is: a frame only decodes from its start.
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
        let total = self.info.total_blocks as u64;
        let Some(target) = u64::try_from(required_ts.get())
            .ok()
            .filter(|target| *target <= total)
        else {
            return seek_error(SeekErrorKind::OutOfRange);
        };

        let blocks_per_frame = u64::from(self.info.header.blocks_per_frame);
        // The end of the file is the end of the last frame, not the start of
        // a frame that does not exist.
        let frame = if target == total {
            self.total_frames() - 1
        } else {
            (target / blocks_per_frame) as u32
        };
        let skip = (target - u64::from(frame) * blocks_per_frame) as usize * self.block_align();

        self.position = target;
        match &mut self.current {
            Some(current) if current.index == frame => current.cursor = skip,
            _ => {
                self.current = None;
                self.next_frame = frame;
                self.next_skip = skip;
                if self.in_flight != Some(frame) {
                    self.abandon_worker();
                }
            }
        }
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
        let align = self.block_align();
        loop {
            if let Some(current) = &mut self.current {
                let left = current.pcm.len().saturating_sub(current.cursor);
                if left > 0 {
                    let bytes = left.min(PACKET_FRAMES * align);
                    let data = current.pcm[current.cursor..current.cursor + bytes].to_vec();
                    current.cursor += bytes;
                    let frames = (bytes / align) as u64;
                    let packet = Packet::new(
                        0,
                        Timestamp::new(self.position as i64),
                        Duration::from(frames),
                        data,
                    );
                    self.position += frames;
                    return Ok(Some(packet));
                }
                self.current = None;
            }
            if self.next_frame >= self.total_frames() {
                return Ok(None);
            }
            if let Err(error_packet) = self.advance()? {
                return Ok(Some(error_packet));
            }
        }
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

/// Positions for mono and stereo, discrete channels above that: the format
/// keeps no channel mask, and a guessed position sends a channel to the wrong
/// speaker.
fn channel_layout(count: u16) -> Channels {
    match count {
        1 => CHANNEL_LAYOUT_MONO,
        2 => CHANNEL_LAYOUT_STEREO,
        other => Channels::Discrete(other),
    }
}

fn sample_format(bits: u16) -> Result<SampleFormat> {
    match bits {
        8 => Ok(SampleFormat::U8),
        16 => Ok(SampleFormat::S16),
        24 => Ok(SampleFormat::S24),
        32 => Ok(SampleFormat::S32),
        _ => unsupported_error("ape: unsupported bit depth"),
    }
}

/// Turns the reader's packets of little-endian PCM into planes.
pub struct ApeDecoder {
    params: AudioCodecParameters,
    buf: GenericAudioBuffer,
    channels: usize,
}

impl ApeDecoder {
    fn try_new(params: &AudioCodecParameters) -> Result<Self> {
        let (Some(rate), Some(channels), Some(format)) = (
            params.sample_rate,
            params.channels.clone(),
            params.sample_format,
        ) else {
            return unsupported_error("ape: incomplete codec parameters");
        };
        let count = channels.count();
        Ok(Self {
            params: params.clone(),
            buf: GenericAudioBuffer::new(format, AudioSpec::new(rate, channels), PACKET_FRAMES),
            channels: count,
        })
    }

    fn decode_inner(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        self.buf.clear();
        // What the reader sends for a frame that failed: see `advance`.
        if packet.data.is_empty() {
            return decode_error("ape: frame failed to decode");
        }
        let channels = self.channels;
        let data = packet.data;
        match &mut self.buf {
            GenericAudioBuffer::U8(buf) => fill(buf, data, channels, |b: &[u8]| b[0]),
            GenericAudioBuffer::S16(buf) => fill(buf, data, channels, |b: &[u8]| {
                i16::from_le_bytes([b[0], b[1]])
            }),
            GenericAudioBuffer::S24(buf) => fill(buf, data, channels, |b: &[u8]| {
                i24(i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8)
            }),
            GenericAudioBuffer::S32(buf) => fill(buf, data, channels, |b: &[u8]| {
                i32::from_le_bytes([b[0], b[1], b[2], b[3]])
            }),
            _ => decode_error("ape: unexpected sample format"),
        }
    }
}

/// Deinterleave a packet into the planes of `buf`.
fn fill<S: Sample>(
    buf: &mut AudioBuffer<S>,
    data: &[u8],
    channels: usize,
    read: impl Fn(&[u8]) -> S,
) -> Result<()> {
    let width = (S::EFF_BITS / 8) as usize;
    let frames = data.len() / (width * channels);
    if frames * width * channels != data.len() {
        return decode_error("ape: packet is not a whole number of frames");
    }
    buf.grow_capacity(frames);
    buf.render_uninit(Some(frames));
    for channel in 0..channels {
        let Some(plane) = buf.plane_mut(channel) else {
            return decode_error("ape: missing audio plane");
        };
        for (frame, sample) in plane.iter_mut().enumerate().take(frames) {
            *sample = read(&data[(frame * channels + channel) * width..]);
        }
    }
    Ok(())
}

impl AudioDecoder for ApeDecoder {
    /// Nothing to forget: the state lives with the reader's worker, which a
    /// seek replaces.
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

impl RegisterableAudioDecoder for ApeDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        _opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(ApeDecoder::try_new(params)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[SupportedAudioCodec {
            id: CODEC_ID_MONKEYS_AUDIO,
            info: CodecInfo {
                short_name: "ape",
                long_name: "Monkey's Audio",
                profiles: &[],
            },
        }]
    }
}
