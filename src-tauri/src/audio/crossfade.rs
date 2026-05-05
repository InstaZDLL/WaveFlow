//! Crossfade DSP helpers.
//!
//! Apart from the equal-power gain curves, this module knows how to
//! open a "secondary" symphonia stream to the same f32 / dst_rate /
//! dst_channels pipeline used for the primary, so [`crate::audio::decoder`]
//! can mix two tracks into the SPSC ring during a fade window without
//! re-implementing the symphonia init dance twice.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::resampler::Resampler;

/// All per-stream decoder state, packaged so the primary stream can be
/// swapped out for the secondary one when a crossfade completes.
pub struct ActiveStream {
    pub format: Box<dyn FormatReader>,
    pub decoder: Box<dyn Decoder>,
    pub sample_buf: Option<SampleBuffer<f32>>,
    pub resampler: Resampler,
    pub src_channels: usize,
    pub symphonia_track_id: u32,
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
        let file = File::open(path).map_err(|e| format!("open: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
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
            format,
            decoder,
            sample_buf: None,
            // Real resampler is built after the first packet (we need
            // the actual source rate, which AAC/M4A only reveals then).
            resampler: Resampler::Passthrough,
            src_channels: 0,
            symphonia_track_id,
            track_id,
            duration_ms,
            source_type,
            source_id,
            replay_gain_linear: replay_gain_db_to_linear(replay_gain_db),
        })
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
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(true);
                }
                Err(SymphoniaError::ResetRequired) => return Ok(true),
                Err(e) => return Err(format!("next_packet: {e}")),
            };
            if packet.track_id() != self.symphonia_track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphoniaError::DecodeError(e)) => {
                    tracing::warn!(error = %e, "decode error, skipping packet");
                    continue;
                }
                Err(e) => return Err(format!("decode fatal: {e}")),
            };
            if self.sample_buf.is_none() {
                let spec = *decoded.spec();
                let capacity = decoded.capacity() as u64;
                self.sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
                self.src_channels = spec.channels.count();
                let src_sample_rate = spec.rate;
                self.resampler =
                    Resampler::new(src_sample_rate, dst_sample_rate, dst_channels)
                        .map_err(|e| format!("resampler init: {e}"))?;
            }
            let sb = self.sample_buf.as_mut().unwrap();
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
