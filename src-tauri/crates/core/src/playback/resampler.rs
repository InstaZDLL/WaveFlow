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
use rubato::{Fft, FixedSync, Resampler as _, WindowFunction};

/// A rubato failure, building the resampler or running a chunk through
/// it. The message names the step that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResamplerError(pub String);

impl std::fmt::Display for ResamplerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ResamplerError {}

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

// `Fft` carries multi-hundred-byte scratch buffers + the rubato
// state, dwarfing `Passthrough`. Boxing would add an indirection on
// every audio packet in the hot path, and the enum is only stored
// once per stream (the primary + the prefetched secondary during
// crossfade), so the size disparity is fine.
#[allow(clippy::large_enum_variant)]
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
        src_rate: u32,
        dst_rate: u32,
        /// Frames given to [`Resampler::process`] and frames it has
        /// returned since the last flush: what [`Resampler::finish`]
        /// measures the end of a track against.
        frames_in: u64,
        frames_out: u64,
    },
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> Result<Self, ResamplerError> {
        if src_rate == dst_rate {
            return Ok(Self::Passthrough);
        }

        // rubato 4.0 dropped `sub_chunks` from `Fft::new` (it now
        // auto-picks ~256-frame sub-chunks); `new_custom` keeps our
        // deliberate SUB_CHUNKS=2 baseline and pins BlackmanHarris2 —
        // the exact window `Fft::new` used internally in 3.0 — so the
        // anti-aliasing response is unchanged across the bump.
        let inner = Fft::<f32>::new_custom(
            src_rate as usize,
            dst_rate as usize,
            CHUNK_SIZE,
            SUB_CHUNKS,
            channels,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map_err(|e| ResamplerError(format!("rubato init: {e}")))?;

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
            src_rate,
            dst_rate,
            frames_in: 0,
            frames_out: 0,
        })
    }

    /// Process an interleaved `f32` input buffer and append the
    /// resampled interleaved output into `out`. Returns `Ok(())` on
    /// success; on rubato errors returns a [`ResamplerError`].
    ///
    /// When [`Self::Passthrough`], the input is appended verbatim.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), ResamplerError> {
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
                frames_in,
                frames_out,
                ..
            } => {
                let chans = *channels;
                debug_assert!(
                    input.len() % chans == 0,
                    "interleaved input not aligned to channel count"
                );

                pending.extend_from_slice(input);
                *frames_in += (input.len() / chans) as u64;

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
                        let in_buf =
                            InterleavedSlice::new(&in_scratch[..in_samples], chans, frames_in)
                                .map_err(|e| ResamplerError(format!("rubato in adapter: {e}")))?;
                        let mut out_buf = InterleavedSlice::new_mut(
                            &mut out_scratch[..frames_out_max * chans],
                            chans,
                            frames_out_max,
                        )
                        .map_err(|e| ResamplerError(format!("rubato out adapter: {e}")))?;
                        let (_n_in, n_out) = inner
                            .process_into_buffer(&in_buf, &mut out_buf, None)
                            .map_err(|e| ResamplerError(format!("rubato process: {e}")))?;
                        n_out
                    };

                    out.extend_from_slice(&out_scratch[..n_out * chans]);
                    *frames_out += n_out as u64;
                }
                Ok(())
            }
        }
    }

    /// At the end of a track, push out what is still inside: the input
    /// short of a whole chunk, and the filter's delay. Without it those
    /// frames were lost whenever the rates differ, up to a chunk plus the
    /// delay (about 25 ms at 48 kHz).
    ///
    /// Zeros are fed until the output reaches its length, then the output
    /// is cut there, so none of them comes out as silence between two
    /// tracks played without a gap. That length is the input's at the
    /// output rate plus the delay: the delay is not trimmed at the start
    /// of a track, so the track's own output runs that much longer.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), ResamplerError> {
        let (target, chunk) = match self {
            Self::Passthrough => return Ok(()),
            Self::Fft {
                inner,
                channels,
                src_rate,
                dst_rate,
                frames_in,
                ..
            } => {
                // Nothing given since it was built or flushed (a speed
                // change right at the end): no tail, and pushing the
                // delay out would only add silence.
                if *frames_in == 0 {
                    return Ok(());
                }
                let scaled = (u128::from(*frames_in) * u128::from(*dst_rate)
                    + u128::from(*src_rate) / 2)
                    / u128::from(*src_rate);
                (
                    inner.output_delay() as u64 + scaled as u64,
                    inner.input_frames_next() * *channels,
                )
            }
        };
        let start = out.len();
        let zeros = vec![0.0_f32; chunk];
        // The delay is shorter than a chunk, so two or three passes do.
        for _ in 0..8 {
            if self.frames_out() >= target {
                break;
            }
            self.process(&zeros, out)?;
        }
        if let Self::Fft {
            channels,
            frames_out,
            ..
        } = self
        {
            let excess = frames_out.saturating_sub(target) as usize * *channels;
            out.truncate(out.len().saturating_sub(excess).max(start));
            *frames_out = (*frames_out).min(target);
        }
        Ok(())
    }

    fn frames_out(&self) -> u64 {
        match self {
            Self::Passthrough => 0,
            Self::Fft { frames_out, .. } => *frames_out,
        }
    }

    /// Drop any frames still buffered in the pending queue, and start
    /// counting the track's length again. The end of a track goes
    /// through [`Self::finish`] instead.
    pub fn flush(&mut self) {
        if let Self::Fft {
            pending,
            frames_in,
            frames_out,
            ..
        } = self
        {
            pending.clear();
            *frames_in = 0;
            *frames_out = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The end of a track comes out whole, and stops at the track's
    /// length: what the resampler held is not lost, and the zeros that
    /// pushed it out do not become a gap before the next track.
    #[test]
    fn finish_brings_the_output_to_the_track_length() {
        let channels = 2;
        let (src, dst) = (48_000_u32, 44_100_u32);
        let mut r = Resampler::new(src, dst, channels).expect("ctor");
        let delay = match &r {
            Resampler::Fft { inner, .. } => inner.output_delay(),
            Resampler::Passthrough => unreachable!(),
        };
        let mut out = Vec::new();
        // One second of a steady level, in pieces that are not chunks.
        for _ in 0..48 {
            r.process(&vec![0.5_f32; 1_000 * channels], &mut out)
                .expect("process");
        }
        let expected = delay + 44_100;
        assert!(out.len() / channels < expected, "the tail is still inside");

        r.finish(&mut out).expect("finish");

        assert_eq!(out.len() / channels, expected);
        // Well inside the signal's end, the level is still there.
        let frame = (expected - 300) * channels;
        assert!((out[frame] - 0.5).abs() < 0.05, "got {}", out[frame]);
    }

    #[test]
    fn finish_with_nothing_given_adds_nothing() {
        let mut r = Resampler::new(48_000, 44_100, 2).expect("ctor");
        let mut out = Vec::new();
        r.finish(&mut out).expect("finish");
        assert!(out.is_empty());
    }

    #[test]
    fn finish_on_passthrough_adds_nothing() {
        let mut r = Resampler::new(44_100, 44_100, 2).expect("ctor");
        let mut out = vec![0.1_f32, 0.2];
        r.finish(&mut out).expect("finish");
        assert_eq!(out, [0.1, 0.2]);
    }

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
