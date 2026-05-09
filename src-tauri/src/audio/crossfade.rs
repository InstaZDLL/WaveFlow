//! Crossfade DSP helpers.
//!
//! Apart from the equal-power gain curves, this module knows how to
//! open a "secondary" symphonia stream to the same f32 / dst_rate /
//! dst_channels pipeline used for the primary, so [`crate::audio::decoder`]
//! can mix two tracks into the SPSC ring during a fade window without
//! re-implementing the symphonia init dance twice.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use super::dsd::parser::{parse_dff, parse_dsf, DsdLayout};
use super::dsd::pcm::DsdToPcm;
use super::resampler::Resampler;

/// Per-stream decoder backend. Symphonia handles the FLAC / MP3 /
/// AAC / WAV / OGG / ALAC family; DSD is a native pipeline because
/// symphonia 0.5 doesn't decode it.
pub enum StreamBackend {
    Symphonia {
        format: Box<dyn FormatReader>,
        decoder: Box<dyn Decoder>,
        sample_buf: Option<SampleBuffer<f32>>,
        symphonia_track_id: u32,
    },
    Dsd {
        file: File,
        layout: DsdLayout,
        converter: DsdToPcm,
        /// Bytes consumed from the data chunk so far. Used to detect
        /// EOF and to compute the byte offset for a seek.
        bytes_read: u64,
        /// Reusable scratch for raw DSD bytes pulled from disk.
        dsd_scratch: Vec<u8>,
        /// Reusable scratch for PCM samples right out of the FIR
        /// converter, before channel conversion. Held here so we can
        /// reuse the allocation across decode calls.
        pcm_src_scratch: Vec<f32>,
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
    /// Linear gain factor derived from `track_analysis.replay_gain_db`
    /// for this track. `1.0` means "no gain known / disabled". Stored
    /// per-stream so the crossfade dual-decoder mix gives each track
    /// its own gain before they are summed.
    pub replay_gain_linear: f32,
}

/// How many DSD bytes to pull from disk per `decode_next` cycle.
/// 64 KiB ≈ 23 ms of DSD64 stereo, comfortably below the SPSC ring
/// capacity but large enough to amortise the FIR convolution cost.
const DSD_READ_CHUNK: usize = 64 * 1024;

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
        replay_gain_db: Option<f64>,
    ) -> Result<Self, String> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());

        // DSD has its own pipeline — symphonia 0.5 doesn't decode it.
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
                replay_gain_db,
            );
        }

        let file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = ext.as_deref() {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(|e| format!("probe: {e}"))?;
        let format = probed.format;
        let track_symphonia = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| "no decodable track".to_string())?;
        let symphonia_track_id = track_symphonia.id;
        let codec_params = track_symphonia.codec_params.clone();
        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|e| format!("codec init: {e}"))?;

        Ok(Self {
            backend: StreamBackend::Symphonia {
                format,
                decoder,
                sample_buf: None,
                symphonia_track_id,
            },
            // Real resampler is built after the first packet (we need
            // the actual source rate, which AAC/M4A only reveals then).
            resampler: Resampler::Passthrough,
            src_channels: 0,
            track_id,
            duration_ms,
            source_type,
            source_id,
            replay_gain_linear: replay_gain_db_to_linear(replay_gain_db),
        })
    }

    /// Build an ActiveStream wrapping the DSD pipeline. The container
    /// is parsed up-front so we know the bit rate, channel count and
    /// where the bitstream starts; the DsdToPcm converter is built
    /// from the layout so its FIR ring buffers match the channel
    /// count.
    fn open_dsd(
        path: &Path,
        ext: &str,
        track_id: i64,
        duration_ms: u64,
        source_type: String,
        source_id: Option<i64>,
        replay_gain_db: Option<f64>,
    ) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let layout = match ext {
            "dsf" => parse_dsf(&mut file).map_err(|e| format!("dsf parse: {e}"))?,
            "dff" => parse_dff(&mut file).map_err(|e| format!("dff parse: {e}"))?,
            _ => return Err(format!("unexpected DSD extension: {ext}")),
        };
        // Position the file cursor at the start of the bitstream so
        // the first decode_next call streams from the right offset.
        file.seek(SeekFrom::Start(layout.data_offset))
            .map_err(|e| format!("dsd seek to data: {e}"))?;
        let converter = DsdToPcm::new(&layout);
        let src_channels = layout.channels.count() as usize;
        Ok(Self {
            backend: StreamBackend::Dsd {
                file,
                layout,
                converter,
                bytes_read: 0,
                dsd_scratch: Vec::with_capacity(DSD_READ_CHUNK),
                pcm_src_scratch: Vec::with_capacity(DSD_READ_CHUNK),
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
            replay_gain_linear: replay_gain_db_to_linear(replay_gain_db),
        })
    }

    /// Symphonia track id for the active backend, if any. DSD has no
    /// equivalent — callers should skip the seek / reset path for it.
    pub fn symphonia_track_id(&self) -> Option<u32> {
        match &self.backend {
            StreamBackend::Symphonia { symphonia_track_id, .. } => Some(*symphonia_track_id),
            StreamBackend::Dsd { .. } => None,
        }
    }

    /// Mutable access to the symphonia format reader for callers that
    /// need to drive `seek`. `None` for DSD — callers branch on the
    /// `Option` and call [`Self::seek_ms`] instead.
    pub fn format_mut(&mut self) -> Option<&mut Box<dyn FormatReader>> {
        match &mut self.backend {
            StreamBackend::Symphonia { format, .. } => Some(format),
            StreamBackend::Dsd { .. } => None,
        }
    }

    /// Reset internal decoder state after a seek. For symphonia this
    /// flushes the codec's frame buffer; for DSD it drops the FIR
    /// history (the high-frequency transient is below the cutoff and
    /// inaudible).
    pub fn reset_decoder(&mut self) {
        match &mut self.backend {
            StreamBackend::Symphonia { decoder, .. } => decoder.reset(),
            StreamBackend::Dsd { layout, converter, .. } => {
                *converter = DsdToPcm::new(layout);
            }
        }
    }

    /// Seek to `ms` milliseconds. Returns `Ok` even when the offset
    /// is past EOF — seeking a VBR file or beyond the DSD bitstream
    /// is best-effort.
    pub fn seek_ms(&mut self, ms: u64) {
        match &mut self.backend {
            StreamBackend::Symphonia {
                format,
                symphonia_track_id,
                ..
            } => {
                let time = Time::from(std::time::Duration::from_millis(ms));
                if let Err(err) = format.seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time,
                        track_id: Some(*symphonia_track_id),
                    },
                ) {
                    tracing::warn!(?err, ms, "format seek failed");
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
                let bps = (layout.sample_rate_hz as u128)
                    * (layout.channels.count() as u128)
                    / 8
                    / 1000;
                let target_data_offset = (ms as u128 * bps) as u64;
                let absolute = layout.data_offset + target_data_offset.min(layout.data_len_bytes);
                if let Err(err) = file.seek(SeekFrom::Start(absolute)) {
                    tracing::warn!(?err, ms, "dsd seek failed");
                    return;
                }
                *bytes_read = target_data_offset.min(layout.data_len_bytes);
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
    pub fn decode_next(
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
                sample_buf,
                symphonia_track_id,
            } => loop {
                let packet = match format.next_packet() {
                    Ok(p) => p,
                    Err(SymphoniaError::IoError(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        return Ok(true);
                    }
                    Err(SymphoniaError::ResetRequired) => return Ok(true),
                    Err(e) => return Err(format!("next_packet: {e}")),
                };
                if packet.track_id() != *symphonia_track_id {
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
                if sample_buf.is_none() {
                    let spec = *decoded.spec();
                    let capacity = decoded.capacity() as u64;
                    *sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
                    self.src_channels = spec.channels.count();
                    let src_sample_rate = spec.rate;
                    self.resampler =
                        Resampler::new(src_sample_rate, dst_sample_rate, dst_channels)
                            .map_err(|e| format!("resampler init: {e}"))?;
                }
                let sb = sample_buf.as_mut().unwrap();
                sb.copy_interleaved_ref(decoded);
                interleaved_scratch.clear();
                super::decoder::convert_channels(
                    sb.samples(),
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
            } => {
                if *bytes_read >= layout.data_len_bytes {
                    return Ok(true);
                }
                // Pull a fixed-size DSD chunk, capped at whatever's
                // left in the data block.
                let remaining = layout.data_len_bytes - *bytes_read;
                let want = remaining.min(DSD_READ_CHUNK as u64) as usize;
                dsd_scratch.resize(want, 0);
                let read = file
                    .read(dsd_scratch)
                    .map_err(|e| format!("dsd read: {e}"))?;
                if read == 0 {
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
                super::decoder::convert_channels(
                    pcm_src_scratch,
                    self.src_channels,
                    dst_channels,
                    interleaved_scratch,
                );

                // Stage 3: resample from converter rate to device rate.
                // Lazily built on first decode now that we know the
                // actual source rate.
                if matches!(self.resampler, Resampler::Passthrough)
                    && converter.output_rate_hz != dst_sample_rate
                {
                    self.resampler = Resampler::new(
                        converter.output_rate_hz,
                        dst_sample_rate,
                        dst_channels,
                    )
                    .map_err(|e| format!("dsd resampler init: {e}"))?;
                }

                self.resampler
                    .process(interleaved_scratch, out)
                    .map_err(|e| format!("dsd resample: {e}"))?;
                Ok(false)
            }
        }
    }
}

/// Convert a ReplayGain dB value into a linear scalar applicable to
/// f32 samples. Returns `1.0` when no gain is known or when the value
/// looks suspicious (NaN, ±∞, beyond ±24 dB) so a buggy analysis row
/// can never blow the speakers.
#[inline]
pub fn replay_gain_db_to_linear(db: Option<f64>) -> f32 {
    match db {
        Some(v) if v.is_finite() && v.abs() <= 24.0 => 10f64.powf(v / 20.0) as f32,
        _ => 1.0,
    }
}

/// Equal-power fade gains for a normalized progress `t ∈ [0, 1]`.
/// Returns `(fade_out_gain, fade_in_gain)` where
/// `fade_out² + fade_in² = 1` so total power is preserved across the
/// mix — gives a noticeably less "scooped" mid-fade than linear gains.
#[inline]
pub fn equal_power_gains(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let angle = t * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn equal_power_endpoints_are_pure() {
        let (out0, in0) = equal_power_gains(0.0);
        assert!(approx_eq(out0, 1.0, 1e-6));
        assert!(approx_eq(in0, 0.0, 1e-6));

        let (out1, in1) = equal_power_gains(1.0);
        assert!(approx_eq(out1, 0.0, 1e-6));
        assert!(approx_eq(in1, 1.0, 1e-6));
    }

    #[test]
    fn equal_power_preserves_energy() {
        // The whole point of equal-power vs linear is that the sum of
        // squares stays at 1 across the fade, so total RMS doesn't dip
        // mid-window. Sample 21 points and assert the invariant.
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let (o, i_) = equal_power_gains(t);
            assert!(
                approx_eq(o * o + i_ * i_, 1.0, 1e-5),
                "energy not preserved at t={t}: {}",
                o * o + i_ * i_
            );
        }
    }

    #[test]
    fn equal_power_clamps_out_of_range_t() {
        // t outside [0, 1] should clamp, not panic or wrap.
        assert_eq!(equal_power_gains(-0.5), equal_power_gains(0.0));
        assert_eq!(equal_power_gains(1.5), equal_power_gains(1.0));
    }

    #[test]
    fn replay_gain_none_is_unity() {
        assert_eq!(replay_gain_db_to_linear(None), 1.0);
    }

    #[test]
    fn replay_gain_zero_db_is_unity() {
        assert!(approx_eq(replay_gain_db_to_linear(Some(0.0)), 1.0, 1e-6));
    }

    #[test]
    fn replay_gain_six_db_doubles_amplitude() {
        // +6 dB ≈ 2× amplitude, −6 dB ≈ ½× — sanity-check both directions.
        assert!(approx_eq(replay_gain_db_to_linear(Some(6.0)), 1.995, 0.01));
        assert!(approx_eq(replay_gain_db_to_linear(Some(-6.0)), 0.501, 0.01));
    }

    #[test]
    fn replay_gain_rejects_pathological_values() {
        // Any analysis row that says "blow the speakers" gets ignored.
        assert_eq!(replay_gain_db_to_linear(Some(f64::NAN)), 1.0);
        assert_eq!(replay_gain_db_to_linear(Some(f64::INFINITY)), 1.0);
        assert_eq!(replay_gain_db_to_linear(Some(-f64::INFINITY)), 1.0);
        assert_eq!(replay_gain_db_to_linear(Some(40.0)), 1.0);
        assert_eq!(replay_gain_db_to_linear(Some(-40.0)), 1.0);
    }
}
