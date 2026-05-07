//! Thin wrapper around rubato's FFT resampler with `FixedSync::Input`
//! semantics: every `process_into_buffer` call must consume exactly
//! `inner.input_frames_next()` interleaved frames.
//!
//! Both symphonia (decoder) and cpal (output) operate on interleaved
//! `f32`, so we feed and read interleaved buffers via the
//! [`InterleavedSlice`] adapter — no deinterleave/reinterleave hop.
//!
//! When the source and destination sample rates already match, a
//! [`Resampler::Passthrough`] variant skips all allocation and just
//! forwards the input slice unchanged — important for the common case
//! where the user's tracks already match the device rate.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler as _};

use crate::error::{AppError, AppResult};

/// Desired FFT chunk size in input frames per call. Rubato may round
/// to the nearest GCD-aligned value of the (in_rate, out_rate) pair;
/// the actual size is queried via `input_frames_next()` after
/// construction. 1024 is the rubato-recommended sweet spot — smaller
/// trades latency for FFT overhead, larger trades memory for
/// frequency resolution.
const CHUNK_SIZE: usize = 1024;

/// FFT sub-chunk count. Higher values reduce processing delay at the
/// cost of softer anti-aliasing; 2 is the value rubato's docs use as
/// a baseline.
const SUB_CHUNKS: usize = 2;

pub enum Resampler {
    Passthrough,
    Fft {
        inner: Fft<f32>,
        channels: usize,
        /// Reusable interleaved input buffer sized to one rubato chunk.
        in_scratch: Vec<f32>,
        /// Reusable interleaved output buffer sized to the resampler's
        /// max output per call.
        out_scratch: Vec<f32>,
        /// Interleaved samples not yet handed to rubato. Drains in
        /// `input_frames_next()` increments on every `process` call.
        pending: Vec<f32>,
    },
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> AppResult<Self> {
        if src_rate == dst_rate {
            return Ok(Self::Passthrough);
        }

        let inner = Fft::<f32>::new(
            src_rate as usize,
            dst_rate as usize,
            CHUNK_SIZE,
            SUB_CHUNKS,
            channels,
            FixedSync::Input,
        )
        .map_err(|e| AppError::Audio(format!("rubato init: {e}")))?;

        let frames_in = inner.input_frames_next();
        let frames_out_max = inner.output_frames_max();
        let in_scratch = vec![0.0_f32; frames_in * channels];
        let out_scratch = vec![0.0_f32; frames_out_max * channels];
        let pending = Vec::with_capacity(frames_in * channels * 2);

        Ok(Self::Fft {
            inner,
            channels,
            in_scratch,
            out_scratch,
            pending,
        })
    }

    /// Process an interleaved `f32` input buffer and append the
    /// resampled interleaved output into `out`. Returns `Ok(())` on
    /// success; on rubato errors returns [`AppError::Audio`].
    ///
    /// When [`Self::Passthrough`], the input is appended verbatim.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> AppResult<()> {
        match self {
            Self::Passthrough => {
                out.extend_from_slice(input);
                Ok(())
            }
            Self::Fft {
                inner,
                channels,
                in_scratch,
                out_scratch,
                pending,
            } => {
                let chans = *channels;
                debug_assert!(
                    input.len() % chans == 0,
                    "interleaved input not aligned to channel count"
                );

                pending.extend_from_slice(input);

                loop {
                    let frames_in = inner.input_frames_next();
                    let in_samples = frames_in * chans;
                    if pending.len() < in_samples {
                        break;
                    }
                    in_scratch[..in_samples].copy_from_slice(&pending[..in_samples]);
                    pending.drain(..in_samples);

                    // `output_frames_max` may shift between calls when
                    // FixedSync::Input pulls multiple sub-chunks from
                    // the saved-frames backlog; resize on the rare
                    // occasion it grows so the adapter always fits.
                    let frames_out_max = inner.output_frames_max();
                    if out_scratch.len() < frames_out_max * chans {
                        out_scratch.resize(frames_out_max * chans, 0.0);
                    }

                    let n_out = {
                        let in_buf = InterleavedSlice::new(
                            &in_scratch[..in_samples],
                            chans,
                            frames_in,
                        )
                        .map_err(|e| AppError::Audio(format!("rubato in adapter: {e}")))?;
                        let mut out_buf = InterleavedSlice::new_mut(
                            &mut out_scratch[..frames_out_max * chans],
                            chans,
                            frames_out_max,
                        )
                        .map_err(|e| AppError::Audio(format!("rubato out adapter: {e}")))?;
                        let (_n_in, n_out) = inner
                            .process_into_buffer(&in_buf, &mut out_buf, None)
                            .map_err(|e| AppError::Audio(format!("rubato process: {e}")))?;
                        n_out
                    };

                    out.extend_from_slice(&out_scratch[..n_out * chans]);
                }
                Ok(())
            }
        }
    }

    /// Drop any frames still buffered in the pending queue. MVP: we
    /// just discard them, which truncates the tail by at most one
    /// rubato chunk. Gapless playback would need
    /// `process_into_buffer(..., partial_len = Some(remaining))` here
    /// so rubato pads with silence instead of swallowing the tail.
    pub fn flush(&mut self) {
        if let Self::Fft { pending, .. } = self {
            pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_returns_input_unchanged() {
        let mut r = Resampler::new(48_000, 48_000, 2).expect("ctor");
        let input = vec![0.1_f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        let mut out = Vec::new();
        r.process(&input, &mut out).expect("process");
        assert_eq!(out, input);
    }

    #[test]
    fn fft_48k_to_44_1k_produces_proportional_output() {
        let channels = 2;
        let src = 48_000_u32;
        let dst = 44_100_u32;
        let mut r = Resampler::new(src, dst, channels).expect("ctor");

        // Feed ~1s of stereo silence in chunks of 4096 frames.
        let total_frames = src as usize;
        let mut out = Vec::new();
        let chunk_frames = 4096;
        let mut fed = 0;
        while fed < total_frames {
            let take = chunk_frames.min(total_frames - fed);
            let buf = vec![0.0_f32; take * channels];
            r.process(&buf, &mut out).expect("process");
            fed += take;
        }

        // Output should be roughly `total_frames * dst / src` interleaved
        // frames. Tolerance covers the trailing partial rubato chunk we
        // haven't drained yet.
        let expected_frames = total_frames * dst as usize / src as usize;
        let produced_frames = out.len() / channels;
        let diff = (produced_frames as i64 - expected_frames as i64).abs();
        assert!(
            diff < 2_048,
            "expected ~{expected_frames} frames, got {produced_frames} (diff {diff})"
        );
        assert_eq!(out.len() % channels, 0, "output not aligned to channels");
    }
}
