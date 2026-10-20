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
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use crate::audio_format::dsd::dop::DsdToDop;
use crate::audio_format::dsd::parser::{parse_dff, parse_dsf, DsdLayout};
use crate::audio_format::dsd::pcm::DsdToPcm;

use super::replay_gain::TrackGain;
use super::resampler::Resampler;

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

/// Read the whole file into RAM and wrap it in a seekable in-memory
/// [`MediaSource`]. Returns `None` (→ caller streams instead) when the
/// file is missing, unreadable, or larger than [`MAX_PRELOAD_BYTES`].
fn read_into_memory(path: &Path) -> Option<Box<dyn MediaSource>> {
    use std::io::Read;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "network pre-load open failed");
            return None;
        }
    };
    let known_len = file.metadata().ok().map(|m| m.len());
    // Early exit when the file is already known to exceed the cap — avoids
    // pulling CAP + 1 bytes off the network just to reject it.
    if known_len.is_some_and(|len| len > MAX_PRELOAD_BYTES) {
        tracing::debug!(
            path = %path.display(),
            "network file exceeds pre-load cap — streaming instead"
        );
        return None;
    }
    // Reserve only when we trust the size; the bounded read below remains
    // the real cap, covering an unknown or post-stat-grown size (no
    // TOCTOU). Read at most CAP + 1 bytes — pulling that many means the
    // file is over the cap regardless of what the stat claimed.
    let mut buf = Vec::with_capacity(known_len.unwrap_or(0) as usize);
    if let Err(e) = file.take(MAX_PRELOAD_BYTES + 1).read_to_end(&mut buf) {
        tracing::warn!(path = %path.display(), error = %e, "network pre-load read failed");
        return None;
    }
    if buf.len() as u64 > MAX_PRELOAD_BYTES {
        tracing::debug!(
            path = %path.display(),
            "network file exceeds pre-load cap — streaming instead"
        );
        return None;
    }
    Some(Box::new(std::io::Cursor::new(buf)))
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
        // high-latency link mid-playback. Pre-load the whole file into
        // RAM once (under a size cap) and decode from an in-memory
        // cursor instead. Best-effort: any failure / oversize falls
        // through to ordinary streaming.
        if is_network_path(path) {
            if let Some(source) = read_into_memory(path) {
                tracing::debug!(path = %path.display(), "network path — pre-loaded into RAM");
                return Self::open_from_source(
                    source,
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
            .map(|registered| codec_label(registered.codec.info.short_name));

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

    /// Build an ActiveStream wrapping the DSD pipeline. The container
    /// is parsed up-front so we know the bit rate, channel count and
    /// where the bitstream starts; the DsdToPcm converter is built
    /// from the layout so its FIR ring buffers match the channel
    /// count. `dop` selects the native DoP passthrough backend over the
    /// DSD → PCM one (#495).
    #[allow(clippy::too_many_arguments)]
    fn open_dsd(
        path: &Path,
        ext: &str,
        track_id: i64,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
        replay_gain: TrackGain,
        dsd_taps: usize,
        dop: bool,
    ) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let layout = match ext {
            "dsf" => parse_dsf(&mut file).map_err(|e| format!("dsf parse: {e}"))?,
            "dff" => parse_dff(&mut file).map_err(|e| format!("dff parse: {e}"))?,
            _ => return Err(format!("unexpected DSD extension: {ext}")),
        };
        // Position the file cursor at the start of the bitstream so
        // the first decode call streams from the right offset.
        file.seek(SeekFrom::Start(layout.data_offset))
            .map_err(|e| format!("dsd seek to data: {e}"))?;
        let src_channels = layout.channels.count() as usize;
        let container_duration_ms = Some(layout.duration_ms());
        let codec = Some(
            layout
                .dsd_rate_multiple()
                .map_or_else(|| "DSD".to_string(), |m| format!("DSD{m}")),
        );
        // DoP path (#495): repackage the raw bitstream instead of
        // decoding it. The engine has already re-opened the output at
        // this stream's DoP rate; here we just build the encoder and
        // let `decode_dop_block` feed 24-bit words straight to the ring.
        if dop {
            let encoder = Box::new(DsdToDop::new(&layout));
            return Ok(Self {
                backend: StreamBackend::Dop {
                    file,
                    layout,
                    encoder,
                    bytes_read: 0,
                    dsd_scratch: Vec::with_capacity(DSD_READ_CHUNK),
                },
                // DoP is bit-exact: no resampler ever engages.
                resampler: Resampler::Passthrough,
                src_channels,
                track_id,
                duration_ms,
                source_type,
                source_id,
                // DoP is bit-exact — no gain stage runs on it at all.
                replay_gain: super::replay_gain::TrackGain::default(),
                src_sample_rate: 0,
                // DoP has already pinned the output's rate: asking for
                // another one on top would fight it.
                declared_sample_rate: None,
                playback_speed: 1.0,
                container_duration_ms,
                codec,
                tail_flushed: false,
            });
        }
        let converter = Box::new(DsdToPcm::new_with_taps(&layout, dsd_taps));
        // What this stream will hand the ring: the DSD bitstream's rate
        // divided down by the converter (44.1 kHz for DSD64, 88.2 for
        // DSD128, …), which is the rate the device should be asked for
        // when the user wants no resampling (#600).
        let declared_sample_rate = Some(converter.output_rate_hz);
        Ok(Self {
            backend: StreamBackend::Dsd {
                file,
                layout,
                converter,
                bytes_read: 0,
                dsd_scratch: Vec::with_capacity(DSD_READ_CHUNK),
                pcm_src_scratch: Vec::with_capacity(DSD_READ_CHUNK),
                taps: dsd_taps,
            },
            // Resampler from DSD output rate (44.1 kHz for DSD64,
            // 88.2 for DSD128, …) to dst is built lazily on the first
            // decode like the symphonia path — keeps the codepath
            // uniform.
            resampler: Resampler::Passthrough,
            src_channels,
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
    /// inaudible).
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
    }

    /// Seek to `ms` milliseconds. Returns `Ok` even when the offset
    /// is past EOF — seeking a VBR file or beyond the DSD bitstream
    /// is best-effort.
    pub fn seek_ms(&mut self, ms: u64) {
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
                    Ok(seeked) => extent.seeked(seeked.actual_ts.get()),
                    Err(err) => tracing::warn!(?err, ms, "format seek failed"),
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
                    return;
                }
                *bytes_read = aligned;
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
                    return;
                }
                *bytes_read = aligned;
                encoder.reset();
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

    /// One packet of [`Self::decode_next`], without the resampler's tail.
    fn decode_packet(
        &mut self,
        out: &mut Vec<f32>,
        interleaved_scratch: &mut Vec<f32>,
        dst_sample_rate: u32,
        dst_channels: usize,
    ) -> Result<bool, String> {
        // Snapshot fields used after `&mut backend` so the borrow
        // checker doesn't complain when we reach for `self.resampler`
        // or `self.src_channels` inside the match arms.
        match &mut self.backend {
            StreamBackend::Symphonia {
                format,
                decoder,
                decoded_interleaved,
                symphonia_track_id,
                spec_captured,
                extent,
            } => loop {
                let packet = match format.next_packet() {
                    Ok(Some(p)) => p,
                    Ok(None) => return Ok(true),
                    Err(SymphoniaError::ResetRequired) => return Ok(true),
                    Err(SymphoniaError::IoError(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof
                            && extent.ends_within_last_packet() =>
                    {
                        tracing::warn!(
                            decoded_to = extent.decoded_to,
                            declared_end = ?extent.declared_end,
                            "stray bytes after the last frame; ending the track there"
                        );
                        return Ok(true);
                    }
                    Err(e) => return Err(format!("next_packet: {e}")),
                };
                if packet.track_id != *symphonia_track_id {
                    continue;
                }
                let decoded = match decoder.decode(&packet) {
                    Ok(d) => d,
                    Err(SymphoniaError::DecodeError(e)) => {
                        tracing::warn!(error = %e, "decode error, skipping packet");
                        continue;
                    }
                    Err(e) => return Err(format!("decode fatal: {e}")),
                };
                // After the decode, not before: a packet skipped as
                // undecodable is audio that did not play.
                extent.read(packet.pts.get(), packet.dur.get());
                if !*spec_captured {
                    let spec = decoded.spec();
                    self.src_channels = spec.channels().count();
                    self.src_sample_rate = spec.rate();
                    let effective_rate =
                        ((spec.rate() as f32) * self.playback_speed).max(1.0) as u32;
                    self.resampler = Resampler::new(effective_rate, dst_sample_rate, dst_channels)
                        .map_err(|e| format!("resampler init: {e}"))?;
                    *spec_captured = true;
                }
                decoded.copy_to_vec_interleaved(decoded_interleaved);
                interleaved_scratch.clear();
                super::channels::convert_channels(
                    decoded_interleaved,
                    self.src_channels,
                    dst_channels,
                    interleaved_scratch,
                );
                self.resampler
                    .process(interleaved_scratch, out)
                    .map_err(|e| format!("resample: {e}"))?;
                return Ok(false);
            },
            StreamBackend::Dsd {
                file,
                layout,
                converter,
                bytes_read,
                dsd_scratch,
                pcm_src_scratch,
                // taps is only needed to rebuild the converter on reset;
                // the decode loop reuses the existing one.
                taps: _,
            } => {
                if *bytes_read >= layout.data_len_bytes {
                    return Ok(true);
                }
                // Pull a fixed-size DSD chunk, capped at whatever's
                // left in the data block. Critically, round the
                // request down to a whole number of interleave
                // strides — for DSF that's block_size × channels
                // (typically 8192 bytes). A truncated last chunk
                // would leave the converter mid-cycle: ch0 would
                // receive its block, ch1 would receive a partial
                // one, and at the next call (or EOF) ch0 / ch1
                // sample counts diverge → channels desynchronise →
                // grit/noise audible during the primary's tail
                // (and through the first ~half of the crossfade
                // because g_out is still > 0 there).
                let stride = interleave_stride(layout);
                let remaining = layout.data_len_bytes - *bytes_read;
                let raw_want = remaining.min(DSD_READ_CHUNK as u64);
                let aligned = raw_want
                    .checked_div(stride)
                    .map(|q| q * stride)
                    .unwrap_or(raw_want);
                // If the leftover at EOF is smaller than one stride,
                // we'd never make progress. In that case treat the
                // residual as EOF — losing < stride / bytes-per-ms
                // ≈ 12 ms is far less perceptible than the grit it
                // would otherwise produce.
                let want = if aligned == 0 {
                    *bytes_read = layout.data_len_bytes;
                    return Ok(true);
                } else {
                    aligned as usize
                };
                dsd_scratch.resize(want, 0);
                let read =
                    read_full_block(file, dsd_scratch).map_err(|e| format!("dsd read: {e}"))?;
                // A short return means the payload ended earlier than
                // the header claimed (truncated file / bad `data_len`),
                // so it can land mid-cycle. Drop the sub-stride
                // remainder rather than feed the converter bytes it
                // would attribute to the wrong channel.
                let read = sub_stride_trim(read, stride);
                if read == 0 {
                    *bytes_read = layout.data_len_bytes;
                    return Ok(true);
                }
                *bytes_read += read as u64;
                dsd_scratch.truncate(read);

                // Stage 1: DSD bitstream → PCM at source channel count
                // and the converter's output rate (DSD64 → 44.1 kHz).
                pcm_src_scratch.clear();
                converter.decode_block(dsd_scratch, pcm_src_scratch);
                if pcm_src_scratch.is_empty() {
                    // The FIR is still priming on the first call —
                    // tell the engine we made progress without
                    // emitting samples so it doesn't think we hit EOF.
                    return Ok(false);
                }

                // Stage 2: channel-convert from DSD's source channels
                // (typically 2) to the device's channel count (often
                // 8 on a 7.1 cpal output). Skipping this step makes
                // the resampler interpret a stereo buffer as 8-channel
                // and read 4× too fast — exactly what you hear as
                // "accelerated + pixelated" playback.
                interleaved_scratch.clear();
                super::channels::convert_channels(
                    pcm_src_scratch,
                    self.src_channels,
                    dst_channels,
                    interleaved_scratch,
                );

                // Stage 3: resample from converter rate to device rate.
                // Lazily built on first decode now that we know the
                // actual source rate.
                if matches!(self.resampler, Resampler::Passthrough)
                    && (converter.output_rate_hz != dst_sample_rate
                        || (self.playback_speed - 1.0).abs() > f32::EPSILON)
                {
                    self.src_sample_rate = converter.output_rate_hz;
                    let effective_rate =
                        ((converter.output_rate_hz as f32) * self.playback_speed).max(1.0) as u32;
                    self.resampler = Resampler::new(effective_rate, dst_sample_rate, dst_channels)
                        .map_err(|e| format!("dsd resampler init: {e}"))?;
                } else if self.src_sample_rate == 0 {
                    // Mark that we've seen the true source rate even
                    // when speed is 1.0 and rates match (passthrough),
                    // so a later rebuild_resampler call knows what to
                    // multiply by.
                    self.src_sample_rate = converter.output_rate_hz;
                }

                self.resampler
                    .process(interleaved_scratch, out)
                    .map_err(|e| format!("dsd resample: {e}"))?;
                Ok(false)
            }
            // A DoP stream is driven exclusively by `decode_dop_block`,
            // which produces raw 24-bit words with no resampling. Reaching
            // the PCM decode path for it is a wiring bug, not a runtime
            // condition — surface it rather than emit silence.
            StreamBackend::Dop { .. } => {
                Err("decode_next called on a DoP stream (use decode_dop_block)".into())
            }
        }
    }

    /// Decode one chunk of a DoP stream into interleaved 24-bit DoP
    /// words (marker in bits 23..16, two DSD bytes below), #495. No FIR,
    /// no resampler, no channel convert — the words go straight to the
    /// ring for a bit-perfect transport to the DAC. Returns `Ok(true)`
    /// at EOF, `Ok(false)` after a successful block.
    ///
    /// Only valid on a [`StreamBackend::Dop`]; other backends return an
    /// error (the caller picks `play_dop_track` vs `play_track` off the
    /// same DoP flag, so this never happens at runtime).
    pub fn decode_dop_block(&mut self, out: &mut Vec<u32>) -> Result<bool, String> {
        let StreamBackend::Dop {
            file,
            layout,
            encoder,
            bytes_read,
            dsd_scratch,
        } = &mut self.backend
        else {
            return Err("decode_dop_block called on a non-DoP stream".into());
        };

        if *bytes_read >= layout.data_len_bytes {
            return Ok(true);
        }
        // Round the read down to a whole interleave stride so the encoder
        // never pairs bytes across a channel boundary (same reasoning as
        // the DSD → PCM path).
        let stride = interleave_stride(layout);
        let remaining = layout.data_len_bytes - *bytes_read;
        let raw_want = remaining.min(DSD_READ_CHUNK as u64);
        let aligned = raw_want
            .checked_div(stride)
            .map(|q| q * stride)
            .unwrap_or(raw_want);
        if aligned == 0 {
            // Sub-stride residual at EOF — dropping < ~12 ms is
            // imperceptible and avoids a torn final frame.
            *bytes_read = layout.data_len_bytes;
            return Ok(true);
        }
        dsd_scratch.resize(aligned as usize, 0);
        let read = read_full_block(file, dsd_scratch).map_err(|e| format!("dop read: {e}"))?;
        // Same guard as the DSD → PCM path: a payload shorter than the
        // header claims can end mid-cycle, and a partial cycle would
        // pair bytes across channels.
        let read = sub_stride_trim(read, stride);
        if read == 0 {
            *bytes_read = layout.data_len_bytes;
            return Ok(true);
        }
        *bytes_read += read as u64;
        dsd_scratch.truncate(read);
        encoder.encode_block(dsd_scratch, out);
        Ok(false)
    }
}

/// Bytes making up one full interleave cycle of a DSD stream: a DSF
/// block per channel, or a single byte per channel for DFF. Every read
/// and every seek must land on a multiple of this, or the decoder starts
/// attributing one channel's bytes to another.
#[inline]
/// A symphonia short name as the format chip spells it.
fn codec_label(short_name: &str) -> String {
    match short_name {
        n if n.starts_with("pcm") => "PCM".into(),
        "vorbis" => "Vorbis".into(),
        "opus" => "Opus".into(),
        n => n.to_ascii_uppercase(),
    }
}

fn interleave_stride(layout: &DsdLayout) -> u64 {
    match layout.block_interleave {
        Some(block) => (block as u64) * (layout.channels.count() as u64),
        None => layout.channels.count() as u64,
    }
}

/// Round a byte target down to a whole interleave stride and clamp it to
/// the payload — the shared half of both DSD seek paths.
#[inline]
fn aligned_byte_offset(target: u64, stride: u64, data_len_bytes: u64) -> u64 {
    target
        .checked_div(stride)
        .map(|q| q * stride)
        .unwrap_or(target)
        .min(data_len_bytes)
}

/// Byte offset of `ms` into a DSD payload.
///
/// The division comes last on purpose. Folding it into a bytes-per-ms
/// constant first truncates the ratio — DSD64 stereo is 705.6 bytes/ms,
/// which becomes 705 — and the error is then multiplied by the seek
/// position: about 0.15 s short three minutes in, and growing from
/// there. u128 throughout so DSD512 stereo can't overflow.
#[inline]
fn dsd_byte_offset_for_ms(layout: &DsdLayout, ms: u64) -> u64 {
    let bits_per_second = (layout.sample_rate_hz as u128) * (layout.channels.count() as u128);
    // Saturate rather than let the `as` truncate: a wrapped offset would
    // land somewhere plausible inside the payload and survive the clamp
    // in `aligned_byte_offset`, which only bounds the top end.
    ((ms as u128) * bits_per_second / 8 / 1000).min(u64::MAX as u128) as u64
}

/// Round a byte count down to a whole interleave stride — what's left of
/// a block that ended mid-cycle is unusable, not just short.
#[inline]
fn sub_stride_trim(read: usize, stride: u64) -> usize {
    ((read as u64 / stride.max(1)) * stride) as usize
}

/// Read exactly `buf.len()` bytes unless the file genuinely ends first.
///
/// `Read::read` may come up short at any time, and for a DSD block that
/// is not a benign truncation: the requested size is a whole interleave
/// stride, so a fragment ends mid-cycle and every byte after it is
/// attributed to the wrong channel — audible as channel-swapped grit on
/// the PCM path, and as a torn frame against the marker cadence on the
/// DoP one. With this loop, a short return means real EOF, which is what
/// both callers already assume.
fn read_full_block<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod extent_tests {
    use super::{frames_to_ticks, StreamExtent};

    #[test]
    fn frames_convert_to_the_track_time_base() {
        // FLAC: the time base is the sample rate's reciprocal.
        assert_eq!(frames_to_ticks(4_608, 44_100, 1, 44_100), Some(4_608));
        // A millisecond time base at 48 kHz: 4 608 frames are 96 ms.
        assert_eq!(frames_to_ticks(4_608, 48_000, 1, 1_000), Some(96));
        assert_eq!(frames_to_ticks(4_608, 0, 1, 1_000), None);
    }

    #[test]
    fn a_millisecond_time_base_measures_in_milliseconds() {
        // 2 s declared as 2 000 ms ticks, a lost 96 ms packet.
        let mut extent = StreamExtent::new(0, Some(2_000), Some(96));
        extent.read(1_808, 96);
        assert!(extent.ends_within_last_packet());
        extent.read(1_000, 96);
        assert!(!extent.ends_within_last_packet());
    }

    #[test]
    fn the_seed_gives_way_to_the_first_real_packet() {
        // A 16 384-frame fallback, then 4 608-frame packets: the window is
        // one real packet, not the fallback.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(16_384));
        extent.read(0, 4_608);
        extent.read(70_000, 4_608);
        assert!(!extent.ends_within_last_packet());
        extent.read(82_944, 4_608);
        assert!(extent.ends_within_last_packet());
    }

    #[test]
    fn stray_bytes_after_the_last_frame_end_the_track() {
        // The report's file: 88 200 frames declared, 4 608-frame packets,
        // the last one lost to the trailing bytes.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.read(82_944, 4_608);
        assert!(extent.ends_within_last_packet());
    }

    #[test]
    fn a_file_cut_short_is_still_an_error() {
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.read(40_000, 4_608);
        assert!(!extent.ends_within_last_packet());
        // Nothing declared, nothing to measure against.
        let mut unknown = StreamExtent::new(0, None, Some(4_608));
        unknown.read(83_000, 4_608);
        assert!(!unknown.ends_within_last_packet());
    }

    #[test]
    fn decoding_well_past_the_declared_end_is_not_its_end() {
        // Declares 88 200 frames, holds more, and is cut at 200 000.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.read(195_392, 4_608);
        assert!(!extent.ends_within_last_packet());
        // Just past it, within a packet, still is.
        extent.read(86_000, 4_608);
        assert!(extent.ends_within_last_packet());
    }

    #[test]
    fn a_seek_into_the_tail_counts_from_where_it_landed() {
        // No packet read yet: the declared maximum is the measure.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.seeked(86_000);
        assert!(extent.ends_within_last_packet());
        // And with no measure at all, the error stands.
        let mut blind = StreamExtent::new(0, Some(88_200), None);
        blind.seeked(86_000);
        assert!(!blind.ends_within_last_packet());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of 44.1 kHz 16-bit stereo PCM, as a WAV file.
    fn write_wav(path: &Path) {
        let (rate, frames, channels) = (44_100_u32, 44_100_u32, 2_u16);
        let data_len = frames * u32::from(channels) * 2;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend((channels * 2).to_le_bytes());
        bytes.extend(16_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_len.to_le_bytes());
        for i in 0..frames {
            let sample = ((i % 100) as i16 - 50) * 300;
            bytes.extend(sample.to_le_bytes());
            bytes.extend(sample.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Played to the end at another rate, a track comes out at its full
    /// length: what the resampler held at the end is not lost.
    #[test]
    fn a_track_resampled_to_its_end_keeps_its_length() {
        use rubato::Resampler as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_wav(&path);
        let mut stream = ActiveStream::open(
            &path,
            0,
            0,
            String::new(),
            None,
            TrackGain::default(),
            64,
            false,
        )
        .unwrap();
        assert_eq!(stream.container_duration_ms, Some(1_000));
        assert_eq!(stream.codec.as_deref(), Some("PCM"));

        let (mut out, mut scratch) = (Vec::new(), Vec::new());
        while !stream
            .decode_next(&mut out, &mut scratch, 48_000, 2)
            .unwrap()
        {}
        // Asked again at the end, it adds nothing more.
        let at_end = out.len();
        assert!(stream
            .decode_next(&mut out, &mut scratch, 48_000, 2)
            .unwrap());
        assert_eq!(out.len(), at_end);

        let delay = match &stream.resampler {
            Resampler::Fft { inner, .. } => inner.output_delay(),
            Resampler::Passthrough => panic!("44.1 kHz to 48 kHz resamples"),
        };
        assert_eq!(out.len() / 2, delay + 48_000);
    }

    #[test]
    fn network_path_heuristic() {
        // Windows UNC + Linux gvfs mounts are network.
        assert!(path_str_is_network(r"\\nas\music\track.flac"));
        assert!(path_str_is_network(
            "/run/user/1000/gvfs/smb-share:server=nas,share=music/track.flac"
        ));
        // Extended-UNC is still network.
        assert!(path_str_is_network(r"\\?\UNC\nas\music\track.flac"));
        // Plain local paths are not.
        assert!(!path_str_is_network("/home/user/Music/track.flac"));
        assert!(!path_str_is_network(r"C:\Users\me\Music\track.flac"));
        // \\?\ and \\.\ namespace prefixes are local, not network.
        assert!(!path_str_is_network(r"\\?\C:\Users\me\Music\track.flac"));
        assert!(!path_str_is_network(r"\\.\C:\Users\me\Music\track.flac"));
        // The UNC exception is \\?\ only — \\.\UNC\ stays local (device).
        assert!(!path_str_is_network(r"\\.\UNC\nas\music\track.flac"));
    }
}
