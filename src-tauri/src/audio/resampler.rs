//! Thin wrapper around `rubato::FftFixedIn<f32>` that handles the
//! deinterleave → resample → reinterleave dance.
//!
//! Symphonia gives us interleaved f32 samples. Rubato works on planar
//! `Vec<Vec<f32>>` (one inner vec per channel). We convert on the fly.
//!
//! When the source and destination sample rates already match, a
//! [`Resampler::passthrough`] variant skips all allocation and just
//! forwards the input slice unchanged — important for the common case
//! where the user's MP3s already match the device rate.

use rubato::{FftFixedIn, Resampler as _};

use crate::error::{AppError, AppResult};

/// FFT chunk size, in input frames per `process()` call. 1024 is the
/// rubato documented sweet spot for FftFixedIn; smaller = lower latency
/// but more FFT overhead, larger = better frequency resolution but more
/// memory.
const CHUNK_SIZE: usize = 1024;

/// FFT sub-chunk count. 2 is the typical value from rubato's docs.
const SUB_CHUNKS: usize = 2;

pub enum Resampler {
    Passthrough,
    Fft {
        inner: FftFixedIn<f32>,
        channels: usize,
        // Planar scratch buffers kept around so each `process` call
        // reuses the allocation. Inner Vecs are re-sized to CHUNK_SIZE
        // on construction.
        in_buf: Vec<Vec<f32>>,
        /// Accumulator of not-yet-processed frames per channel —
        /// rubato requires exactly CHUNK_SIZE frames per call, so we
        /// buffer partial packets across decoder iterations.
        pending: Vec<Vec<f32>>,
    },
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> AppResult<Self> {
        if src_rate == dst_rate {
            return Ok(Self::Passthrough);
        }

        let inner = FftFixedIn::<f32>::new(
            src_rate as usize,
            dst_rate as usize,
            CHUNK_SIZE,
            SUB_CHUNKS,
            channels,
        )
        .map_err(|e| AppError::Audio(format!("rubato init: {e}")))?;

        let in_buf = vec![vec![0.0_f32; CHUNK_SIZE]; channels];
        let pending = vec![Vec::with_capacity(CHUNK_SIZE * 2); channels];

        Ok(Self::Fft {
            inner,
            channels,
            in_buf,
            pending,
        })
    }

    /// Process an interleaved `f32` input buffer and append resampled
    /// interleaved output into `out`. Returns `Ok(())` on success; on
    /// rubato errors returns [`AppError::Audio`].
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
                in_buf,
                pending,
            } => {
                let chans = *channels;
                debug_assert!(input.len() % chans == 0, "interleaved input not aligned to channel count");
                let frames = input.len() / chans;

                // Deinterleave incoming frames into the per-channel
                // pending buffers.
                for ch in 0..chans {
                    pending[ch].reserve(frames);
                    for f in 0..frames {
                        pending[ch].push(input[f * chans + ch]);
                    }
                }

                // Drain as many full CHUNK_SIZE blocks as pending holds.
                while pending[0].len() >= CHUNK_SIZE {
                    for ch in 0..chans {
                        // Copy one chunk out of pending into the scratch
                        // in_buf, then remove those frames from pending.
                        in_buf[ch].clear();
                        in_buf[ch].extend_from_slice(&pending[ch][..CHUNK_SIZE]);
                        pending[ch].drain(..CHUNK_SIZE);
                    }

                    let resampled = inner
                        .process(in_buf, None)
                        .map_err(|e| AppError::Audio(format!("rubato process: {e}")))?;

                    // Re-interleave into `out`. rubato gives us the same
                    // channel count in the output.
                    let out_frames = resampled[0].len();
                    out.reserve(out_frames * chans);
                    for f in 0..out_frames {
                        for ch in 0..chans {
                            out.push(resampled[ch][f]);
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Flush any frames still buffered in the rubato state. MVP: we
    /// just drop them, which truncates the tail by at most
    /// `CHUNK_SIZE - 1` frames. Gapless playback would need a proper
    /// `process_partial_into_buffer` call here.
    pub fn flush(&mut self) {
        if let Self::Fft { pending, .. } = self {
            for chan in pending.iter_mut() {
                chan.clear();
            }
        }
    }
}
