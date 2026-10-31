//! One track being decoded: open it, decode it packet by packet into
//! interleaved `f32` at the output's rate and channel count, seek in it.
//!
//! Symphonia covers the FLAC / MP3 / AAC / WAV / OGG / ALAC family (Opus
//! through the shared codec registry), DSD is converted to PCM here, and
//! DoP repackages it for a DAC that takes it natively. A player opens one
//! [`ActiveStream`] per track, two during a crossfade, so the same code
//! decodes the primary and the track fading in.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use crate::audio_format::dsd::dop::DsdToDop;
use crate::audio_format::dsd::parser::DsdLayout;
use crate::audio_format::dsd::pcm::DsdToPcm;

use super::read_ahead::ReadAhead;
use super::replay_gain::TrackGain;
use super::resampler::Resampler;

mod decode;
mod dsd;

use dsd::{aligned_byte_offset, dsd_byte_offset_for_ms, dsd_ms_for_byte_offset, interleave_stride};

/// How long the container says the track is, and how far decoding got
/// (#898).
///
/// Some FLAC files carry a few stray bytes after their last frame. The
/// FLAC reader has to find the next frame header to know where the last
/// frame ends, so it reports an unexpected end of file instead of the
/// end of the stream, and the last frame is lost with it — measured on
/// the report's file: 87 552 of 88 200 declared frames decoded, then
/// `UnexpectedEof`, against a clean `Ok(None)` at 88 200 without the
/// tail. Treated as a failure, the whole song had played and the queue
/// still did not move on.
///
/// So an unexpected end of file that lands within one packet of the
/// declared end is the end of the track. Anywhere earlier it is still a
/// truncated file, and still an error; with no declared length there is
/// nothing to measure against, and it stays an error too.
///
/// Everything here is in the track's time base, the unit of a packet's
/// `pts` and `dur`. That is the sample rate's reciprocal for FLAC, but not
/// for every container (Matroska counts milliseconds), so lengths given
/// in frames are converted on the way in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamExtent {
    /// `start_ts` plus the declared length, when the container gives one.
    declared_end: Option<i64>,
    /// End of the last packet read, or where a seek landed.
    decoded_to: i64,
    /// The longest packet seen, seeded with the codec's declared maximum
    /// (or a fallback) so a seek straight to the tail still has a measure.
    longest_packet: u64,
    /// Whether a packet has been read: until then `longest_packet` is the
    /// seed, and the first real packet replaces it rather than letting a
    /// generous fallback widen the window for the rest of the track.
    measured: bool,
}

impl StreamExtent {
    /// `declared_len` and `packet_len` in time-base ticks.
    fn new(start_ts: i64, declared_len: Option<u64>, packet_len: Option<u64>) -> Self {
        Self {
            declared_end: declared_len
                .and_then(|n| i64::try_from(n).ok())
                .map(|n| start_ts.saturating_add(n)),
            decoded_to: start_ts,
            longest_packet: packet_len.unwrap_or(0),
            measured: false,
        }
    }

    fn read(&mut self, pts: i64, dur: u64) {
        self.decoded_to = pts.saturating_add(i64::try_from(dur).unwrap_or(i64::MAX));
        self.longest_packet = if self.measured {
            self.longest_packet.max(dur)
        } else {
            dur
        };
        self.measured = true;
    }

    fn seeked(&mut self, ts: i64) {
        self.decoded_to = ts;
    }

    /// Whether an unexpected end of file here is the end of the track.
    ///
    /// Within one packet either side: a container that declares less than
    /// it holds and is cut further on is still a truncated file, not one
    /// that ended past its end.
    fn ends_within_last_packet(&self) -> bool {
        match self.declared_end {
            Some(end) if self.longest_packet > 0 => {
                end.abs_diff(self.decoded_to) <= self.longest_packet
            }
            _ => false,
        }
    }
}

/// The packet length to measure with before any packet has been read
/// (a seek straight into the tail), when the codec declares none, which
/// symphonia's FLAC reader does not: 16 384 frames, the largest block the
/// FLAC streamable subset allows, about 0.37 s at 44.1 kHz.
const FALLBACK_PACKET_FRAMES: u64 = 16_384;

/// `frames` of audio at `rate` Hz, in ticks of a `numer / denom` second
/// time base: `frames / rate` seconds, divided by the tick length.
fn frames_to_ticks(frames: u64, rate: u32, numer: u32, denom: u32) -> Option<u64> {
    if rate == 0 || numer == 0 {
        return None;
    }
    let ticks = u128::from(frames) * u128::from(denom) / (u128::from(rate) * u128::from(numer));
    u64::try_from(ticks).ok()
}

/// Per-stream decoder backend. Symphonia handles the FLAC / MP3 /
/// AAC / WAV / OGG / ALAC family; DSD is a native pipeline because
/// symphonia doesn't decode it.
pub enum StreamBackend {
    Symphonia {
        format: Box<dyn FormatReader>,
        decoder: Box<dyn AudioDecoder>,
        /// Reusable interleaved f32 destination for
        /// `GenericAudioBufferRef::copy_to_vec_interleaved`. Replaces the
        /// old `SampleBuffer<f32>` field — symphonia 0.6 removed
        /// `SampleBuffer`; the equivalent is now a plain Vec<f32> that
        /// the decoded buffer fills via the conversion helpers on the
        /// `Audio` trait.
        decoded_interleaved: Vec<f32>,
        symphonia_track_id: u32,
        spec_captured: bool,
        extent: StreamExtent,
    },
    Dsd {
        file: File,
        layout: DsdLayout,
        // Boxed to keep this variant from dwarfing the (already
        // all-boxed) Symphonia one — the FIR converter holds several
        // per-channel history buffers that grow with the tap count.
        converter: Box<DsdToPcm>,
        /// Bytes consumed from the data chunk so far. Used to detect
        /// EOF and to compute the byte offset for a seek.
        bytes_read: u64,
        /// Reusable scratch for raw DSD bytes pulled from disk.
        dsd_scratch: Vec<u8>,
        /// Reusable scratch for PCM samples right out of the FIR
        /// converter, before channel conversion. Held here so we can
        /// reuse the allocation across decode calls.
        pcm_src_scratch: Vec<f32>,
        /// FIR tap count this converter was built with. Cached so
        /// `reset_decoder` (post-seek) rebuilds at the same precision
        /// the stream was opened with instead of falling back to the
        /// default.
        taps: usize,
    },
    /// Native DSD passthrough via DoP (DSD over PCM), #495. No FIR, no
    /// resampler, no channel convert: the raw 1-bit stream is repackaged
    /// into 24-bit DoP words and shipped straight to a DoP-capable DAC.
    /// Only reachable when the engine has (re)opened the output at the
    /// exact DoP format — driven by [`ActiveStream::decode_dop_block`],
    /// never by [`ActiveStream::decode_next`].
    Dop {
        file: File,
        layout: DsdLayout,
        encoder: Box<DsdToDop>,
        /// Bytes consumed from the data chunk so far (EOF + seek math).
        bytes_read: u64,
        /// Reusable scratch for raw DSD bytes pulled from disk.
        dsd_scratch: Vec<u8>,
    },
}

/// All per-stream decoder state, packaged so the primary stream can be
/// swapped out for the secondary one when a crossfade completes.
pub struct ActiveStream {
    pub backend: StreamBackend,
    pub resampler: Resampler,
    pub src_channels: usize,
    pub track_id: i64,
    pub duration_ms: u64,
    pub source_type: String,
    pub source_id: Option<i64>,
    /// What is known about this track's loudness — from its own tags
    /// when it has them, from our analysis otherwise. Kept as metadata
    /// rather than as a baked-in scalar so the pre-amp and clipping
    /// settings apply the moment they change, and stored per-stream so
    /// the crossfade dual-decoder mix gives each track its own gain
    /// before the two are summed.
    pub replay_gain: super::replay_gain::TrackGain,
    /// True source sample rate of the underlying audio (44100, 48000,
    /// 96000, …). Captured the first time `decode_next` builds the
    /// resampler and held so `rebuild_resampler` can recompute the
    /// effective input rate (`src * speed`) when the user changes
    /// playback speed mid-track. `0` before the first packet is
    /// decoded.
    pub src_sample_rate: u32,
    /// The rate the container declares for this track, known **before
    /// the first packet** — which is what makes it usable for the
    /// per-track output re-open (#600), where the device has to be
    /// asked for a rate before anything is decoded.
    ///
    /// `None` when the container does not say: AAC in MP4 reveals its
    /// rate only once decoding starts, and a guess there would re-clock
    /// the device to the wrong number. [`Self::src_sample_rate`] is the
    /// measured truth and stays the one the resampler uses.
    pub declared_sample_rate: Option<u32>,
    /// Active playback speed used the last time the resampler was
    /// built. Mirrored here so the decoder loop can skip rebuilds
    /// when nothing changed.
    pub playback_speed: f32,
    /// How long the container says the track is, read at open. For a
    /// player with no library to ask; the desktop app keeps the
    /// `duration_ms` its library supplies. `None` when the container
    /// does not say.
    pub container_duration_ms: Option<u64>,
    /// The codec as the format chip spells it (`FLAC`, `MP3`, `Opus`,
    /// `PCM`, `DSD64`, …), `None` when the registry has no name for it.
    pub codec: Option<String>,
    /// The resampler's tail has gone out after the end of the stream.
    tail_flushed: bool,
}

/// Packets one `decode_next` call may pass over (another track's,
/// undecodable, or a stream change) before it hands back with nothing, so a damaged file still
/// lets the player's commands through: stop, seek, the next track.
const MAX_SKIPPED_PACKETS: usize = 64;

/// How many DSD bytes to pull from disk per `decode_next` cycle.
/// 64 KiB ≈ 23 ms of DSD64 stereo, comfortably below the SPSC ring
/// capacity but large enough to amortise the FIR convolution cost.
const DSD_READ_CHUNK: usize = 64 * 1024;

/// Upper bound on a network file we'll pull fully into RAM. A single
/// hi-res FLAC track tops out well under this; the cap is a guard
/// against accidentally loading a multi-GB file (e.g. a whole-album
/// rip) into memory — those fall back to streaming.
const MAX_PRELOAD_BYTES: u64 = 512 * 1024 * 1024;

/// String-level network-path heuristic, platform-independent so it can
/// be unit-tested: Windows UNC (`\\server\share`) and the gvfs / gio
/// FUSE mount prefixes Linux desktops expose for SMB / SFTP / WebDAV.
fn path_str_is_network(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix(r"\\") {
        // `\\?\` and `\\.\` are Win32 namespace prefixes for *local*
        // access (extended-length / device paths) — NOT network. The one
        // exception is the extended-UNC form `\\?\UNC\server\share`
        // (only valid under `\\?\`, never `\\.\`). A plain
        // `\\server\share` is a genuine UNC network path.
        if let Some(ext) = rest.strip_prefix(r"?\") {
            return ext.to_ascii_uppercase().starts_with("UNC\\");
        }
        if rest.starts_with(r".\") {
            return false; // \\.\ device namespace — always local
        }
        return true;
    }
    let lower = s.to_ascii_lowercase();
    lower.contains("/gvfs/") || lower.contains("/.gvfs/") || lower.contains("smb-share")
}

/// Heuristic: does `path` live on a network filesystem where streaming
/// many small reads during playback risks stutter? Drives the decision
/// to pre-load the file into RAM before decoding.
///
/// Covers UNC + gvfs (via [`path_str_is_network`]) and, on Windows,
/// mapped network drives (`GetDriveTypeW == DRIVE_REMOTE`). Detection is
/// best-effort — an NFS/SMB share mounted at a plain local-looking path
/// (e.g. `/mnt/nas`) is treated as local and just streams.
fn is_network_path(path: &Path) -> bool {
    if path_str_is_network(&path.to_string_lossy()) {
        return true;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

        // `DRIVE_REMOTE` from winbase.h, by value: `GetDriveTypeW`'s
        // numeric contract is fixed.
        const DRIVE_REMOTE: u32 = 4;

        let s = path.to_string_lossy();
        let bytes = s.as_bytes();
        // A drive-letter path (`Z:\…`) → query the volume root's type.
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let root = format!("{}:\\", bytes[0] as char);
            let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: `wide` is a NUL-terminated UTF-16 string that
            // outlives the call.
            let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
            if kind == DRIVE_REMOTE {
                return true;
            }
        }
    }
    false
}

/// Start pulling a network file into RAM in the background
/// ([`ReadAhead`]). `None` (→ the caller streams it) when the file cannot
/// be opened, its size is unknown, or it is larger than
/// [`MAX_PRELOAD_BYTES`].
fn read_ahead(path: &Path) -> Option<ReadAhead> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "network pre-load open failed");
            return None;
        }
    };
    let len = file.metadata().ok()?.len();
    if len > MAX_PRELOAD_BYTES {
        tracing::debug!(
            path = %path.display(),
            "network file exceeds pre-load cap — streaming instead"
        );
        return None;
    }
    // A second handle for reads far ahead of the transfer; without one
    // they wait for it.
    let direct = File::open(path)
        .ok()
        .map(|f| Box::new(f) as Box<dyn super::read_ahead::RandomAccess>);
    // Exactly the length announced as the source's, even if the file
    // grows meanwhile.
    match ReadAhead::spawn(file.take(len), direct, len) {
        Ok(source) => Some(source),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "network pre-load thread failed");
            None
        }
    }
}

impl ActiveStream {
    /// Open `path`, probe + build the codec, and stash everything
    /// needed to feed the resample/channel-convert pipeline. The
    /// resampler stays in `Passthrough` until the first packet is
    /// decoded — only then do we know the source sample rate.
    pub fn open(
        path: &Path,
        track_id: i64,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
        replay_gain: TrackGain,
        dsd_taps: usize,
        dop: bool,
    ) -> Result<Self, String> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());

        // DSD has its own pipeline — symphonia doesn't decode it.
        // Branch up-front so we never hand a DSF / DFF file to the
        // probe (which would fail with a confusing "unknown format"
        // even though the file is valid).
        if matches!(ext.as_deref(), Some("dsf") | Some("dff")) {
            return Self::open_dsd(
                path,
                ext.as_deref().unwrap_or(""),
                track_id,
                duration_ms,
                source_type,
                source_id,
                replay_gain,
                dsd_taps,
                dop,
            );
        }

        // Files on a network share (SMB / NFS / mapped drive / gvfs)
        // stutter when symphonia issues many small reads + seeks over a
        // high-latency link mid-playback. Pull the whole file into RAM
        // (under a size cap) in large sequential reads, on a thread of its
        // own so this call returns once the header is in, and decode from
        // that buffer as it fills. Best-effort: any failure / oversize
        // falls through to ordinary streaming.
        if is_network_path(path) {
            if let Some(source) = read_ahead(path) {
                tracing::debug!(path = %path.display(), "network path — reading ahead into RAM");
                return Self::open_from_source(
                    Box::new(source),
                    ext.as_deref(),
                    track_id,
                    duration_ms,
                    source_type,
                    source_id,
                    replay_gain,
                );
            }
        }

        let file = File::open(path).map_err(|e| format!("open: {e}"))?;
        Self::open_from_source(
            Box::new(file),
            ext.as_deref(),
            track_id,
            duration_ms,
            source_type,
            source_id,
            replay_gain,
        )
    }

    /// Probe + decode any `MediaSource`. Shared between the file-backed
    /// path (`open(path)` above) and the live-stream path
    /// (`audio::http_source::HttpMediaSource` for Web Radio).
    ///
    /// `ext_hint` is forwarded to the probe so symphonia ranks the
    /// correct demuxer first when the source's bytes don't lead with an
    /// unambiguous magic — Icecast servers often serve "audio/mpeg"
    /// streams that need the "mp3" hint to probe cleanly.
    ///
    /// `duration_ms = 0` is the canonical "live stream" marker — the
    /// decoder loop reads this to suppress the end-of-track guard, the
    /// crossfade prefetch hand-off, and the auto-advance round trip.
    pub fn open_from_source(
        source: Box<dyn MediaSource>,
        ext_hint: Option<&str>,
        track_id: i64,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
        replay_gain: TrackGain,
    ) -> Result<Self, String> {
        let mss = MediaSourceStream::new(source, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = ext_hint {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| format!("probe: {e}"))?;
        let track_symphonia = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| "no decodable track".to_string())?;
        let symphonia_track_id = track_symphonia.id;
        let audio_params = track_symphonia
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| "track has no audio codec params".to_string())?;
        // Read before the reader is moved into the struct below: the
        // params borrow it. `None` for a container that does not say —
        // AAC in MP4 only reveals its rate once decoding starts (#600).
        let declared_sample_rate = audio_params.sample_rate;
        // Frames to time-base ticks, the unit packets are stamped in.
        let to_ticks = |frames: u64| -> Option<u64> {
            let base = track_symphonia.time_base?;
            frames_to_ticks(
                frames,
                declared_sample_rate?,
                base.numer.get(),
                base.denom.get(),
            )
        };
        // `duration` is already in ticks; `num_frames` is the fallback.
        let declared_len = track_symphonia
            .duration
            .map(|d| d.get())
            .or_else(|| track_symphonia.num_frames.and_then(to_ticks));
        let extent = StreamExtent::new(
            track_symphonia.start_ts.get(),
            declared_len,
            to_ticks(
                audio_params
                    .max_frames_per_packet
                    .unwrap_or(FALLBACK_PACKET_FRAMES),
            ),
        );
        // The container's own length, read the way the scanner's probe
        // reads it.
        let container_duration_ms = track_symphonia
            .duration
            .and_then(|d| track_symphonia.time_base.and_then(|tb| tb.calc_duration(d)))
            .map(|t| (t.as_micros() / 1000) as u64)
            .or_else(|| {
                track_symphonia
                    .num_frames
                    .zip(declared_sample_rate.filter(|r| *r > 0))
                    .map(|(n, r)| n.saturating_mul(1000) / u64::from(r))
            });
        // The app's registry, not symphonia's: it is the one that
        // carries Opus (#581).
        let registry = crate::audio_format::opus::codecs();
        let decoder = registry
            .make_audio_decoder(audio_params, &AudioDecoderOptions::default())
            .map_err(|e| format!("codec init: {e}"))?;
        let codec = registry
            .get_audio_decoder(audio_params.codec)
            .map(|registered| crate::audio_format::codec_label(registered.codec.info.short_name));

        Ok(Self {
            backend: StreamBackend::Symphonia {
                format,
                decoder,
                decoded_interleaved: Vec::new(),
                symphonia_track_id,
                spec_captured: false,
                extent,
            },
            // Real resampler is built after the first packet (we need
            // the actual source rate, which AAC/M4A only reveals then).
            resampler: Resampler::Passthrough,
            src_channels: 0,
            track_id,
            duration_ms,
            source_type,
            source_id,
            replay_gain,
            src_sample_rate: 0,
            declared_sample_rate,
            playback_speed: 1.0,
            container_duration_ms,
            codec,
            tail_flushed: false,
        })
    }

    /// Rebuild the resampler so its effective input rate is
    /// `src_sample_rate * speed`. Called by the decoder loop when the
    /// user changes playback speed mid-track. No-op when called before
    /// the first packet has been decoded (i.e. `src_sample_rate == 0`)
    /// — the lazy init path inside `decode_next` will pick up the
    /// active speed via `self.playback_speed`.
    ///
    /// Returns the new effective source rate so the caller can log
    /// the actual rubato configuration. The old resampler's pending
    /// queue is discarded; that costs at most one rubato chunk
    /// (~21 ms at 48 kHz) of audio, well below perceptible.
    pub fn rebuild_resampler(
        &mut self,
        speed: f32,
        dst_sample_rate: u32,
        dst_channels: usize,
    ) -> Result<u32, String> {
        self.playback_speed = speed;
        if self.src_sample_rate == 0 {
            return Ok(0);
        }
        let effective = ((self.src_sample_rate as f32) * speed).max(1.0) as u32;
        self.resampler = Resampler::new(effective, dst_sample_rate, dst_channels)
            .map_err(|e| format!("resampler rebuild ({effective}→{dst_sample_rate}): {e}"))?;
        Ok(effective)
    }

    /// Symphonia track id for the active backend, if any. DSD has no
    /// equivalent — callers should skip the seek / reset path for it.
    pub fn symphonia_track_id(&self) -> Option<u32> {
        match &self.backend {
            StreamBackend::Symphonia {
                symphonia_track_id, ..
            } => Some(*symphonia_track_id),
            StreamBackend::Dsd { .. } | StreamBackend::Dop { .. } => None,
        }
    }

    /// Mutable access to the symphonia format reader for callers that
    /// need to drive `seek`. `None` for DSD — callers branch on the
    /// `Option` and call [`Self::seek_ms`] instead.
    pub fn format_mut(&mut self) -> Option<&mut Box<dyn FormatReader>> {
        match &mut self.backend {
            StreamBackend::Symphonia { format, .. } => Some(format),
            StreamBackend::Dsd { .. } | StreamBackend::Dop { .. } => None,
        }
    }

    /// Reset internal decoder state after a seek. For symphonia this
    /// flushes the codec's frame buffer; for DSD it drops the FIR
    /// history (the high-frequency transient is below the cutoff and
    /// inaudible). The resampler forgets its queue and its filter
    /// history too ([`Resampler::reset`]).
    pub fn reset_decoder(&mut self) {
        match &mut self.backend {
            StreamBackend::Symphonia { decoder, .. } => decoder.reset(),
            StreamBackend::Dsd {
                layout,
                converter,
                taps,
                ..
            } => {
                **converter = DsdToPcm::new_with_taps(layout, *taps);
            }
            StreamBackend::Dop { encoder, .. } => encoder.reset(),
        }
        // What the resampler holds is the old position too.
        self.resampler.reset();
    }

    /// Seek to `ms` milliseconds, and return where decoding resumes, in
    /// milliseconds. Best-effort: an offset past EOF, a VBR file or one
    /// beyond the DSD bitstream still lands somewhere.
    ///
    /// The landing is at or before `ms`: symphonia's accurate seek stops
    /// on the packet that holds `ms` and the decoder plays that packet
    /// from its start, and a DSD seek rounds down to a whole interleave
    /// cycle. A failed seek reports `ms`, the position the caller asked
    /// for, since nothing better is known.
    pub fn seek_ms(&mut self, ms: u64) -> u64 {
        // A seek back from the end makes the track's end come again.
        self.tail_flushed = false;
        match &mut self.backend {
            StreamBackend::Symphonia {
                format,
                symphonia_track_id,
                extent,
                ..
            } => {
                // Symphonia 0.6 dropped `Time::from(Duration)` /
                // `Time::new(secs, frac)`; the closest replacement is
                // `Time::try_from_secs_f64`, which constructs a
                // (seconds, nanoseconds) pair from a fractional-seconds
                // f64. A negative/non-finite ms is impossible here (u64
                // input), so the option is always `Some`; we fall back
                // to t=0 just to keep the call infallible.
                let time = Time::try_from_secs_f64(ms as f64 / 1000.0).unwrap_or_default();
                match format.seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time,
                        track_id: Some(*symphonia_track_id),
                    },
                ) {
                    Ok(seeked) => {
                        extent.seeked(seeked.actual_ts.get());
                        let landed = format
                            .tracks()
                            .iter()
                            .find(|t| t.id == *symphonia_track_id)
                            .and_then(|t| t.time_base)
                            .and_then(|tb| tb.calc_time(seeked.actual_ts))
                            .map(|t| (t.as_micros() / 1000) as u64);
                        landed.map_or(ms, |l| l.min(ms))
                    }
                    Err(err) => {
                        tracing::warn!(?err, ms, "format seek failed");
                        ms
                    }
                }
            }
            StreamBackend::Dsd {
                file,
                layout,
                bytes_read,
                ..
            } => {
                // Bytes per ms of DSD: rate × channels / 8 / 1000.
                // u128 to dodge overflow on DSD512 stereo.
                let target = dsd_byte_offset_for_ms(layout, ms);
                // Align to a full interleave cycle. For DSF the
                // bitstream is block-interleaved (block_size bytes
                // per channel, looped), so a seek that lands inside
                // a block makes the converter read ch1 bytes as ch0
                // (and vice versa) — audible as channel-swap noise
                // / pixelation after every seek. For DFF the bytes
                // alternate one-by-one across channels, so any
                // multiple of channel-count is safe.
                let stride = interleave_stride(layout);
                let aligned = aligned_byte_offset(target, stride, layout.data_len_bytes);
                let absolute = layout.data_offset + aligned;
                if let Err(err) = file.seek(SeekFrom::Start(absolute)) {
                    tracing::warn!(?err, ms, "dsd seek failed");
                    return ms;
                }
                *bytes_read = aligned;
                dsd_ms_for_byte_offset(layout, aligned)
            }
            StreamBackend::Dop {
                file,
                layout,
                bytes_read,
                encoder,
                ..
            } => {
                // Same byte math as the DSD path — align to a full
                // interleave stride so the encoder never pairs bytes
                // across a channel boundary — then drop the encoder's
                // marker phase + pending byte so the DAC re-locks cleanly.
                let target = dsd_byte_offset_for_ms(layout, ms);
                let stride = interleave_stride(layout);
                let aligned = aligned_byte_offset(target, stride, layout.data_len_bytes);
                let absolute = layout.data_offset + aligned;
                if let Err(err) = file.seek(SeekFrom::Start(absolute)) {
                    tracing::warn!(?err, ms, "dop seek failed");
                    return ms;
                }
                *bytes_read = aligned;
                encoder.reset();
                dsd_ms_for_byte_offset(layout, aligned)
            }
        }
    }

    /// Decode one packet from this stream and append the resampled,
    /// channel-converted f32 samples (interleaved at `dst_channels`)
    /// into `out`. Returns `Ok(true)` on EOF, `Ok(false)` on a
    /// successful packet decode.
    ///
    /// `interleaved_scratch` is a caller-owned reusable buffer for the
    /// channel-conversion stage so we don't re-allocate per packet.
    ///
    /// At the end of the stream, what the resampler still holds is
    /// appended once, cut to the track's length ([`Resampler::finish`]),
    /// along with the `Ok(true)`: the caller plays what `out` holds
    /// before it acts on the end.
    pub fn decode_next(
        &mut self,
        out: &mut Vec<f32>,
        interleaved_scratch: &mut Vec<f32>,
        dst_sample_rate: u32,
        dst_channels: usize,
    ) -> Result<bool, String> {
        let at_end = self.decode_packet(out, interleaved_scratch, dst_sample_rate, dst_channels)?;
        if at_end && !self.tail_flushed {
            self.tail_flushed = true;
            self.resampler
                .finish(out)
                .map_err(|e| format!("resampler tail: {e}"))?;
        }
        Ok(at_end)
    }
}

#[cfg(test)]
mod tests;
