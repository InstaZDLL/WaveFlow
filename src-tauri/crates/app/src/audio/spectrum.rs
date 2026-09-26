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

use crate::host::{AppHandle, Emitter};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use super::state::SharedPlayback;

/// Number of frames analysed per FFT pass. 4096 @ 44.1 kHz is a
/// 10.8 Hz bin — at 2048 (21.5 Hz) the twelve bass bands between 30 and
/// 144 Hz were fed by six distinct bins, so they moved in identical
/// pairs (#715). The window is ~93 ms long, but it slides by [`HOP`], so
/// a new frame is still ready every ~23 ms.
const FFT_SIZE: usize = 4096;

/// How far the window slides between two frames. Fixed rather than
/// `FFT_SIZE / 2`: doubling the window must not halve the update rate.
const HOP: usize = 1024;

/// How fast the level reference follows the music, up and down. It rises
/// within a fraction of a second so a loud passage does not pin every bar
/// to the top, and sinks over seconds so a quiet bar in a loud song stays
/// quiet instead of being inflated between two beats.
const LEVEL_ATTACK: Duration = Duration::from_millis(250);
const LEVEL_RELEASE: Duration = Duration::from_secs(3);

/// The level reference never drops below this magnitude. Without a floor
/// a fade-out or a silent gap would be scaled up until its noise filled
/// the display; with it, a quietly mastered track gets about 6x the gain
/// the old fixed reference gave it, and no more.
const MIN_LEVEL: f32 = 40.0;

/// Room above the reference: the bars reach the top on the peaks of the
/// music, not on its average.
const HEADROOM: f32 = 1.15;

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
    /// Raw (unnormalised) band magnitudes of the current frame.
    raw: Vec<f32>,
    /// Running level reference the bands are scaled against (#715).
    level: f32,
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
            raw: vec![0.0; BAND_COUNT],
            level: MIN_LEVEL,
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
        // A new track starts from the floor, not from the last one's
        // loudness: a quiet track after a loud one would otherwise stay
        // dim for seconds.
        self.level = MIN_LEVEL;
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
            // Same reason for the level: re-enabled mid-track, the old
            // reference would dim the bars for seconds while it decays.
            self.level = MIN_LEVEL;
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
                let elapsed = now
                    .duration_since(self.last_emit)
                    .min(Duration::from_secs(1));
                self.run_one_frame(sample_rate, elapsed);
                let payload = SpectrumPayload {
                    bands: self.bands.clone(),
                };
                let _ = app.emit(EVENT_NAME, payload);
                self.last_emit = now;
            }
            // Slide the window by HOP, so consecutive passes overlap by
            // three quarters. Keeps the visualizer feeling continuous
            // rather than strobing.
            if self.pending.len() > HOP {
                self.pending.drain(..HOP);
            } else {
                self.pending.clear();
            }
        }
    }

    fn run_one_frame(&mut self, sample_rate: f32, elapsed: Duration) {
        // Apply the Hann window into the FFT input buffer.
        for (i, dst) in self.input.iter_mut().enumerate() {
            *dst = self.pending[i] * self.window[i];
        }
        // Infallible barring a length mismatch we control, so the
        // result is logged-and-dropped rather than propagated — the
        // decoder thread shouldn't die because the visualizer
        // hiccuped on a degenerate buffer.
        if let Err(err) =
            self.plan
                .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
        {
            tracing::warn!(?err, "spectrum FFT failed");
            return;
        }

        band_magnitudes(&self.spectrum, sample_rate, &mut self.raw);
        let frame_peak = self.raw.iter().copied().fold(0.0f32, f32::max);
        self.level = follow_level(self.level, frame_peak, elapsed);
        scale_bands(&self.raw, self.level, &mut self.bands);
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

/// Move the level reference toward this frame's loudest band: quickly
/// when it is louder, slowly when it is quieter, never below
/// [`MIN_LEVEL`]. One-pole smoothing, with the coefficient derived from
/// the real time since the previous frame so the feel does not depend on
/// the sample rate or on how the decoder happens to chunk its output.
fn follow_level(level: f32, frame_peak: f32, elapsed: Duration) -> f32 {
    let tau = if frame_peak > level {
        LEVEL_ATTACK
    } else {
        LEVEL_RELEASE
    };
    let alpha = 1.0 - (-elapsed.as_secs_f32() / tau.as_secs_f32()).exp();
    (level + (frame_peak - level) * alpha).max(MIN_LEVEL)
}

/// Magnitude of each log-spaced band: the peak bin inside it, or, for a
/// band narrower than a bin, the spectrum read at the band's centre
/// frequency by linear interpolation between the two nearest bins.
///
/// The peak rather than the mean: averaging across 20+ bins crushes the
/// very transients that make a visualizer feel alive. The interpolation
/// is what keeps two adjacent narrow bands from reporting the same bin —
/// they read the slope between bins at two different points instead.
fn band_magnitudes(spectrum: &[Complex<f32>], sample_rate: f32, bands: &mut [f32]) {
    let bin_count = spectrum.len();
    if bin_count == 0 {
        bands.fill(0.0);
        return;
    }
    let bin_hz = sample_rate / (FFT_SIZE as f32);
    let log_min = MIN_HZ.ln();
    let log_max = MAX_HZ.ln();
    let band_count = bands.len();

    for (b, band) in bands.iter_mut().enumerate() {
        let lo_hz = (log_min + (log_max - log_min) * b as f32 / band_count as f32).exp();
        let hi_hz = (log_min + (log_max - log_min) * (b + 1) as f32 / band_count as f32).exp();
        let lo = lo_hz / bin_hz;
        let hi = hi_hz / bin_hz;

        if hi - lo < 1.0 {
            // Narrower than a bin: read the spectrum at the geometric
            // centre of the band.
            let centre = (lo * hi).sqrt();
            let i = (centre.floor() as usize).min(bin_count - 1);
            let j = (i + 1).min(bin_count - 1);
            let t = centre - i as f32;
            *band = spectrum[i].norm() * (1.0 - t) + spectrum[j].norm() * t;
            continue;
        }

        // Every bin whose centre falls inside the band. At least one
        // does, since the band is a bin wide or more.
        let lo_bin = lo.ceil() as usize;
        let hi_bin = (hi.floor() as usize + 1).max(lo_bin + 1).min(bin_count);
        let mut peak_sq = 0.0f32;
        for bin_val in spectrum.iter().take(hi_bin).skip(lo_bin) {
            peak_sq = peak_sq.max(bin_val.norm_sqr());
        }
        *band = peak_sq.sqrt();
    }
}

/// Squash raw band magnitudes into 0..1 against the level reference.
///
/// The reference used to be a fixed `250.0`, "a loud bin on typical
/// music", so a quietly mastered track — or one that spreads its energy
/// rather than concentrating it — never came near the top. Scaling
/// against the track's own recent level (with [`HEADROOM`] above it)
/// makes the peaks of the music reach the top whatever the master.
///
/// Above [`KNEE`] the scale bends instead of stopping: a transient
/// louder than the reference — the reference takes a fraction of a
/// second to catch up with it — used to be clipped to 1, so every band
/// of the hit landed on the same flat plateau at the top of the display.
/// Compressed, they approach the top without reaching it, and the loudest
/// of them still stands above its neighbours.
///
/// A small floor then treats the bottom as silence, so quantisation and
/// decoder rounding show as zero rather than a constant haze, and a
/// `sqrt` curve expands the low end, where the ear is most sensitive to
/// loudness changes.
fn scale_bands(raw: &[f32], level: f32, bands: &mut [f32]) {
    const FLOOR: f32 = 0.02;
    let reference = level.max(MIN_LEVEL) * HEADROOM;
    for (band, &mag) in bands.iter_mut().zip(raw) {
        let normalised = soft_limit((mag / reference).max(0.0));
        let cut = (normalised - FLOOR).max(0.0) / (1.0 - FLOOR);
        *band = cut.sqrt();
    }
}

/// Where the scale starts to bend. Below it a band is linear in its
/// magnitude, as before.
const KNEE: f32 = 0.8;

/// Identity up to [`KNEE`], then a hyperbolic approach to 1: continuous
/// and with the same slope at the knee, so nothing jumps where the
/// compression begins, and never reaching 1 however loud the band is.
///
/// Hyperbolic rather than exponential: an exponential is within `f32`
/// rounding of 1 by a few times the reference, which is exactly the
/// transient this is for, and two bands would land on the same value
/// again.
fn soft_limit(x: f32) -> f32 {
    if x <= KNEE {
        return x;
    }
    let room = 1.0 - KNEE;
    let over = (x - KNEE) / room;
    KNEE + room * over / (1.0 + over)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hann_window_is_symmetric_and_unit_peak() {
        // Use an odd length so the centre index is an integer and the
        // window hits exactly 1.0: w[(n-1)/2] = 0.5*(1-cos(π)) = 1.0.
        // An even-length window (e.g. 8) has its theoretical peak
        // between two samples (~0.9505 for n=8), which would never
        // satisfy a 1e-3 tolerance.
        let w = hann_window(9);
        // First and last samples are zero, middle peaks at 1.0.
        assert!(w[0] < 1e-6);
        assert!(w[w.len() - 1] < 1e-6);
        let max = w.iter().copied().fold(f32::MIN, f32::max);
        assert!((max - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_spectrum_produces_zero_bands() {
        let mut bands = vec![1.0; BAND_COUNT];
        band_magnitudes(&[], 44_100.0, &mut bands);
        assert!(bands.iter().all(|&v| v == 0.0));
    }

    /// The #715 defect: adjacent bass bands reading the very same bin.
    /// A sloped spectrum must give every band its own value.
    #[test]
    fn no_two_bass_bands_read_the_same_value() {
        let spectrum: Vec<Complex<f32>> = (0..=FFT_SIZE / 2)
            .map(|i| Complex::new(1000.0 / (1.0 + i as f32), 0.0))
            .collect();
        let mut bands = vec![0.0; BAND_COUNT];
        band_magnitudes(&spectrum, 44_100.0, &mut bands);
        for pair in bands[..12].windows(2) {
            assert!(
                (pair[0] - pair[1]).abs() > 1e-3,
                "adjacent bass bands must differ: {pair:?}"
            );
        }
    }

    #[test]
    fn a_quiet_master_still_reaches_the_top() {
        // A frame peak far below the old fixed reference of 250.
        let mut level = MIN_LEVEL;
        for _ in 0..60 {
            level = follow_level(level, 60.0, EMIT_INTERVAL);
        }
        let mut bands = [0.0; 1];
        scale_bands(&[60.0], level, &mut bands);
        assert!(bands[0] > 0.9, "got {}", bands[0]);
    }

    #[test]
    fn silence_is_not_amplified_into_noise() {
        let mut level = 400.0;
        for _ in 0..600 {
            level = follow_level(level, 0.5, EMIT_INTERVAL);
        }
        assert!(level >= MIN_LEVEL);
        let mut bands = [1.0; 1];
        scale_bands(&[0.5], level, &mut bands);
        assert_eq!(bands[0], 0.0);
    }

    #[test]
    fn a_single_beat_does_not_reset_the_scale() {
        // Rising takes a fraction of a second: one loud frame moves the
        // reference only part of the way, so the beat itself reaches the
        // top of the scale.
        let level = follow_level(100.0, 1000.0, EMIT_INTERVAL);
        assert!(level < 300.0, "got {level}");
    }

    /// A hit several times louder than the reference used to clip every
    /// band of it to 1 — a flat plateau at the top of the display. The
    /// louder band now still stands above the other, and neither reaches
    /// the top.
    #[test]
    fn bands_louder_than_the_reference_stay_apart() {
        let mut bands = [0.0; 2];
        scale_bands(&[200.0, 400.0], MIN_LEVEL, &mut bands);
        assert!(bands[0] < bands[1], "got {bands:?}");
        assert!(bands[1] < 1.0, "got {bands:?}");
        assert!(bands[0] > 0.9, "got {bands:?}");
    }

    #[test]
    fn the_soft_limit_is_continuous_at_the_knee() {
        assert_eq!(soft_limit(KNEE), KNEE);
        assert!((soft_limit(KNEE + 1e-3) - (KNEE + 1e-3)).abs() < 1e-5);
        assert_eq!(soft_limit(0.3), 0.3);
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
