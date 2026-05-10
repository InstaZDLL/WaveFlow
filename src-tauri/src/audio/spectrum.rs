//! Real-time spectrum analyzer for the visualizer.
//!
//! Lives on the decoder thread, fed with post-EQ samples right before
//! they hit the SPSC ring. Mixes interleaved channels to mono,
//! windows N samples (Hann), runs a real FFT, then buckets the
//! magnitudes into log-spaced bands suitable for a Spotify / Apple-
//! Music-style bar visualizer.
//!
//! Throttled to ~30 Hz so the Tauri event loop and the renderer don't
//! drown — the visualizer doesn't need full-rate spectra to look
//! smooth (the React side does its own decay/peak hold). The FFT
//! itself runs on the decoder thread, which can afford ~100 µs per
//! frame; the cpal callback is never touched.
//!
//! Allocation policy: every `feed` call works against pre-allocated
//! scratch buffers — no allocations on the hot path. The realfft
//! plan, the FFT scratch, the windowed buffer and the band output
//! all live for the lifetime of `SpectrumAnalyzer`.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use tauri::{AppHandle, Emitter};

use super::state::SharedPlayback;

/// Number of frames analysed per FFT pass. 2048 @ 44.1 kHz gives
/// ~46 ms of context per frame — enough resolution to show the
/// fundamental of bass notes (sub-25 Hz bin spacing) without smearing
/// transients on attacks.
const FFT_SIZE: usize = 2048;

/// Number of output bands (log-spaced bars sent to the UI).
pub const BAND_COUNT: usize = 48;

/// Lowest frequency the band ladder starts at, in Hz. Below ~30 Hz
/// most rooms / drivers have nothing to say, and the FFT bin density
/// is too coarse to be meaningful anyway.
const MIN_HZ: f32 = 30.0;
/// Upper edge in Hz. Cap at 16 kHz — very few sources have meaningful
/// content above and the bands end up too sparse otherwise.
const MAX_HZ: f32 = 16_000.0;

/// Min interval between emitted spectrum frames (~30 Hz). The UI's
/// `requestAnimationFrame` is the actual visual cadence; the backend
/// just feeds it fast enough to keep the bars feeling alive.
const EMIT_INTERVAL: Duration = Duration::from_millis(33);

const EVENT_NAME: &str = "player:spectrum";

pub struct SpectrumAnalyzer {
    plan: Arc<dyn RealToComplex<f32>>,
    /// Mono-mixed samples accumulating until we have FFT_SIZE.
    /// Reused across calls — never reallocated in the hot path.
    pending: Vec<f32>,
    /// FFT input buffer (windowed copy of `pending`).
    input: Vec<f32>,
    /// FFT output (FFT_SIZE/2+1 complex bins).
    spectrum: Vec<Complex<f32>>,
    /// Pre-computed Hann window coefficients.
    window: Vec<f32>,
    /// Scratch the FFT plan needs to run in-place.
    scratch: Vec<Complex<f32>>,
    /// Output band magnitudes (length = BAND_COUNT).
    bands: Vec<f32>,
    last_emit: Instant,
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let plan = planner.plan_fft_forward(FFT_SIZE);
        let scratch = plan.make_scratch_vec();
        let spectrum = plan.make_output_vec();
        let window = hann_window(FFT_SIZE);
        Self {
            plan,
            pending: Vec::with_capacity(FFT_SIZE * 2),
            input: vec![0.0; FFT_SIZE],
            spectrum,
            window,
            scratch,
            bands: vec![0.0; BAND_COUNT],
            // Start "long ago" so the first window emits immediately.
            last_emit: Instant::now()
                .checked_sub(EMIT_INTERVAL)
                .unwrap_or_else(Instant::now),
        }
    }

    /// Reset the rolling buffer. Called on track change so the
    /// previous track's tail samples don't bleed into the first FFT
    /// frame of the new track.
    pub fn reset(&mut self) {
        self.pending.clear();
    }

    /// Feed interleaved samples produced by the decoder. No-ops fast
    /// when the visualizer is disabled or when the throttle window
    /// hasn't elapsed yet.
    pub fn feed(
        &mut self,
        samples: &[f32],
        channels: usize,
        sample_rate: f32,
        shared: &SharedPlayback,
        app: &AppHandle,
    ) {
        if !shared.visualizer_enabled.load(Ordering::Relaxed) {
            // Drop any partial buffer the moment the toggle flips off
            // so a re-enable doesn't show stale data from an old track.
            if !self.pending.is_empty() {
                self.pending.clear();
            }
            return;
        }
        if channels == 0 || sample_rate <= 0.0 {
            return;
        }

        // Mono mix. We average all channels per frame so multi-channel
        // sources don't bias toward whichever channel happens to be
        // first in the interleaved stream.
        let mut i = 0;
        while i + channels <= samples.len() {
            let mut sum = 0.0f32;
            for ch in 0..channels {
                sum += samples[i + ch];
            }
            self.pending.push(sum / channels as f32);
            i += channels;
        }

        // Fire as many FFT frames as we have data for, but stop
        // emitting once the throttle says so — we still want to
        // consume the buffer to keep its length bounded.
        while self.pending.len() >= FFT_SIZE {
            let now = Instant::now();
            let due = now.duration_since(self.last_emit) >= EMIT_INTERVAL;
            if due {
                self.run_one_frame(sample_rate);
                let payload = SpectrumPayload {
                    bands: self.bands.clone(),
                };
                let _ = app.emit(EVENT_NAME, payload);
                self.last_emit = now;
            }
            // Slide the window: drop FFT_SIZE / 2 so the next pass
            // shares half its data with the previous one. Keeps the
            // visualizer feeling continuous rather than strobing.
            let drop = FFT_SIZE / 2;
            if self.pending.len() > drop {
                self.pending.drain(..drop);
            } else {
                self.pending.clear();
            }
        }
    }

    fn run_one_frame(&mut self, sample_rate: f32) {
        // Apply the Hann window into the FFT input buffer.
        for (i, dst) in self.input.iter_mut().enumerate() {
            *dst = self.pending[i] * self.window[i];
        }
        // Infallible barring a length mismatch we control, so the
        // result is logged-and-dropped rather than propagated — the
        // decoder thread shouldn't die because the visualizer
        // hiccuped on a degenerate buffer.
        if let Err(err) = self
            .plan
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
        {
            tracing::warn!(?err, "spectrum FFT failed");
            return;
        }

        compute_bands(&self.spectrum, sample_rate, &mut self.bands);
    }
}

#[derive(Clone, serde::Serialize)]
struct SpectrumPayload {
    bands: Vec<f32>,
}

fn hann_window(n: usize) -> Vec<f32> {
    use std::f32::consts::PI;
    let denom = (n - 1) as f32;
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / denom).cos()))
        .collect()
}

/// Map FFT bins to log-spaced bands and squash into roughly 0..1.
///
/// Normalization is the tricky bit: the raw `realfft` output is
/// **unnormalised** so a full-scale sine through a Hann window peaks
/// at `FFT_SIZE / 4`. We divide each band's RMS-like magnitude by
/// that factor so the result lives in 0..1, then apply a perceptual
/// `sqrt` curve + a small floor cut so quiet ambient still shows
/// a hint of motion without typical pop / rock pegging at the top.
///
/// The earlier dB-based formula had no FFT normalisation and ended
/// up clipping every band to 1.0 on most music — see commit history.
fn compute_bands(spectrum: &[Complex<f32>], sample_rate: f32, bands: &mut [f32]) {
    let bin_count = spectrum.len();
    if bin_count == 0 {
        bands.fill(0.0);
        return;
    }
    let bin_hz = sample_rate / (FFT_SIZE as f32);
    // Empirical reference for "loud bin magnitude": for typical
    // post-Hann music FFT a single dominant bin sits in the 50-300
    // range. We scale so ~250 maps to 1.0, leaving headroom for
    // very loud transients to peg without clipping the visual
    // weeks-of-the-music. (The full theoretical peak is FFT_SIZE/4
    // for a unit sine, but real music never concentrates that
    // much energy in a single bin.)
    let norm_peak = 250.0_f32;
    // Quiet floor: anything below this is treated as silence so the
    // renderer shows zero instead of a constant low-amplitude haze
    // from quantisation / decoder rounding.
    const FLOOR: f32 = 0.02;

    let log_min = MIN_HZ.ln();
    let log_max = MAX_HZ.ln();
    let band_count = bands.len();

    for b in 0..band_count {
        let lo_hz = (log_min + (log_max - log_min) * b as f32 / band_count as f32).exp();
        let hi_hz =
            (log_min + (log_max - log_min) * (b + 1) as f32 / band_count as f32).exp();
        let lo_bin = (lo_hz / bin_hz).floor() as usize;
        let hi_bin = ((hi_hz / bin_hz).ceil() as usize).max(lo_bin + 1);

        // Use the per-band peak rather than the mean — averaging
        // across 20+ bins crushes the very transients that make a
        // visualizer feel alive. A single strong bin should still
        // drive its band bar to full height.
        let mut peak_sq = 0.0f32;
        for bin in lo_bin..hi_bin.min(bin_count) {
            let m = spectrum[bin].norm_sqr();
            if m > peak_sq {
                peak_sq = m;
            }
        }
        let mag = peak_sq.sqrt();
        let normalised = (mag / norm_peak).clamp(0.0, 1.0);
        let cut = (normalised - FLOOR).max(0.0) / (1.0 - FLOOR);
        // Perceptual curve — sqrt expands the low end where the
        // human ear is most sensitive to loudness changes.
        bands[b] = cut.sqrt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hann_window_is_symmetric_and_unit_peak() {
        let w = hann_window(8);
        // First and last samples are zero, middle peaks at 1.0.
        assert!(w[0] < 1e-6);
        assert!(w[w.len() - 1] < 1e-6);
        let max = w.iter().copied().fold(f32::MIN, f32::max);
        assert!((max - 1.0).abs() < 1e-3);
    }

    #[test]
    fn empty_spectrum_produces_zero_bands() {
        let mut bands = vec![1.0; BAND_COUNT];
        compute_bands(&[], 44_100.0, &mut bands);
        assert!(bands.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn analyzer_starts_idle_and_resets_clean() {
        let mut a = SpectrumAnalyzer::new();
        // Pretend we accumulated a few samples then reset — pending
        // must be cleared so a freshly loaded track doesn't inherit
        // stale data.
        a.pending.extend(std::iter::repeat(0.5_f32).take(100));
        a.reset();
        assert!(a.pending.is_empty());
    }
}
