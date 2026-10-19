//! 6-band peaking-EQ DSP.
//!
//! Six biquad peaking filters in series at fixed ISO-ish frequencies
//! (60 / 150 / 400 / 1000 / 2400 / 15000 Hz). Gains are atomic so the
//! UI can move sliders live; coefficients are recomputed lazily in the
//! decoder thread when the dirty flag is set.
//!
//! Applied in the decoder thread (next to ReplayGain) instead of the
//! cpal callback so the callback's no-alloc / no-branch invariant
//! stays clean. Bypass is a single atomic check — when off, the
//! `process()` call is a no-op and adds a single load + branch per
//! buffer (not per sample).
//!
//! Filter math: RBJ Audio EQ Cookbook (peaking band-pass, second-order
//! biquad). Direct Form II Transposed for the per-sample loop —
//! numerically stable enough for f32 at audio rates and one register
//! shorter than DF1.
//!
//! **Headroom.** A boost raises the signal past full scale on a loud
//! master, and the decoder's last stage clamps it to ±1.0: +12 dB of bass
//! on a modern mastered track was heard as hard clipping. So the chain is
//! preceded by a preamp equal to the peak of its own response, read off
//! the cascade itself rather than the largest slider — adjacent bands
//! overlap and their boosts add up — plus 1 dB for transients. A setting
//! with no boost has a preamp of exactly 1.0 and leaves the signal
//! untouched.

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicI16, Ordering};

/// Centre frequencies for each band, the usual six-band consumer layout,
/// so the sliders sit where users expect them.
pub const BAND_FREQS: [f32; 6] = [60.0, 150.0, 400.0, 1000.0, 2400.0, 15000.0];

/// Number of bands. Pulled into a const so the array sizes stay in
/// sync everywhere.
pub const BAND_COUNT: usize = BAND_FREQS.len();

/// Q factor for every band. ~1/sqrt(2) gives ~1-octave bandwidth at
/// the centre frequency — wide enough that adjacent bands overlap
/// naturally, narrow enough that cuts/boosts feel surgical.
const BAND_Q: f32 = 0.707;

/// Maximum cap (absolute value) for each band's gain in dB, the usual
/// ±12 dB of a consumer EQ. The atomic stores tenths of dB (i16 range
/// fits up to ±3276 dB so we have plenty of head-room).
pub const MAX_GAIN_DB: f32 = 12.0;

/// Compute a peaking-EQ biquad coefficient set per the RBJ cookbook.
/// Returns `[b0, b1, b2, a1, a2]` already normalised by `a0` so the
/// per-sample loop doesn't need to divide.
fn peaking_biquad(freq_hz: f32, sample_rate: f32, gain_db: f32, q: f32) -> [f32; 5] {
    let a = 10f32.powf(gain_db / 40.0);
    let omega = 2.0 * PI * (freq_hz / sample_rate);
    let sin_w = omega.sin();
    let cos_w = omega.cos();
    let alpha = sin_w / (2.0 * q);

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_w;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_w;
    let a2 = 1.0 - alpha / a;
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// Peak gain (linear) of the whole cascade, read over the audible band.
///
/// Each biquad's magnitude is evaluated on the unit circle and multiplied
/// in, over 1024 log-spaced points from 20 Hz to 20 kHz (or just under
/// Nyquist) plus every band's centre, where a lone boost peaks. In `f64`:
/// this runs once per change of setting or sample rate, never per sample,
/// on the decoder thread. Measured at about 60 µs in a release build, at
/// 44.1 to 192 kHz alike, so a slider dragged across its range costs the
/// decoder nothing it would notice with a second of audio queued.
fn cascade_peak(coeffs: &[[f32; 5]; BAND_COUNT], sample_rate: f32) -> f32 {
    let rate = f64::from(sample_rate);
    if rate <= 0.0 {
        return 1.0;
    }
    let top = 20_000.0_f64.min(rate * 0.49);
    let low = 20.0_f64.min(top);
    const POINTS: usize = 1024;
    let sweep = (0..POINTS).map(|i| low * (top / low).powf(i as f64 / (POINTS - 1) as f64));
    let centres = BAND_FREQS
        .iter()
        .map(|&f| f64::from(f))
        .filter(|&f| f < rate * 0.5);
    let mut peak = 0.0_f64;
    for freq in sweep.chain(centres) {
        let w = 2.0 * std::f64::consts::PI * freq / rate;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let mut gain = 1.0_f64;
        for c in coeffs {
            let [b0, b1, b2, a1, a2] = c.map(f64::from);
            let num_re = b0 + b1 * c1 + b2 * c2;
            let num_im = -(b1 * s1 + b2 * s2);
            let den_re = 1.0 + a1 * c1 + a2 * c2;
            let den_im = -(a1 * s1 + a2 * s2);
            gain *=
                ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt();
        }
        peak = peak.max(gain);
    }
    peak as f32
}

/// Extra attenuation, as a linear factor (-1 dB), for a boosting setting.
///
/// The response peak bounds a steady signal, not a transient: a filter
/// can overshoot on a sharp full-scale pattern. Measured with full-scale
/// `[1, -1, -1, 1, …]` input, the worst case is +12 dB at 15 kHz, which
/// overshoots its response peak by 0.9 dB. The strict bound (the sum of
/// the impulse response) would cost every boosting setting 2 to 4 dB more
/// for patterns music does not contain, so the margin covers the measured
/// worst case instead, and the decoder's final clamp stays the backstop.
const TRANSIENT_MARGIN: f32 = 0.891_250_9;

/// The preamp that keeps the cascade at or under unity: `1 / peak`, less
/// the transient margin, for a boosting setting; exactly 1.0 for one that
/// only cuts or is flat.
fn headroom_preamp(coeffs: &[[f32; 5]; BAND_COUNT], sample_rate: f32) -> f32 {
    let peak = cascade_peak(coeffs, sample_rate);
    // A flat chain computes a hair over 1.0 in floating point; anything
    // within 0.01 dB of unity is no boost.
    if peak > 1.001 {
        TRANSIENT_MARGIN / peak
    } else {
        1.0
    }
}

/// Scale an interleaved buffer by a gain moving linearly from `start` to
/// `target` across its frames, every channel of a frame alike. With
/// `start == target` the gain is that value throughout, so a steady 1.0
/// leaves the samples bit for bit.
fn apply_preamp(buf: &mut [f32], channels: usize, start: f32, target: f32) {
    let frames = buf.len() / channels.max(1);
    if frames == 0 || (start == 1.0 && target == 1.0) {
        return;
    }
    let step = (target - start) / frames as f32;
    for (f, frame) in buf.chunks_exact_mut(channels).enumerate() {
        let gain = if start == target {
            target
        } else {
            start + step * (f + 1) as f32
        };
        for sample in frame {
            *sample *= gain;
        }
    }
}

/// Per-channel, per-band biquad memory (Direct Form II Transposed).
/// Two state variables per filter — the bare minimum.
#[derive(Debug, Default, Clone, Copy)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

impl BiquadState {
    #[inline(always)]
    fn process(&mut self, x: f32, c: &[f32; 5]) -> f32 {
        let y = c[0] * x + self.z1;
        self.z1 = c[1] * x - c[3] * y + self.z2;
        self.z2 = c[2] * x - c[4] * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Atomic shared state — written by the Tauri commands when the user
/// moves a slider, read by the decoder thread once per packet.
pub struct EqShared {
    /// Gain per band in tenths of dB (e.g. `-30` = `-3.0 dB`). i16
    /// because Atomic*Float is unstable; integer storage avoids the
    /// usual f32-as-bits dance for a domain that doesn't need
    /// sub-tenth precision.
    bands_db_x10: [AtomicI16; BAND_COUNT],
    /// Master bypass. Off (default) skips DSP entirely.
    enabled: AtomicBool,
    /// Set by any band/preset write so the next decoder iteration
    /// recomputes coefficients. Cleared with `swap(false)`.
    dirty: AtomicBool,
}

impl EqShared {
    pub fn new() -> Self {
        Self {
            bands_db_x10: Default::default(),
            enabled: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Release);
        // Mark dirty too: when re-enabling, the coefficients haven't
        // necessarily been recomputed against the current sample rate
        // since the last toggle.
        self.dirty.store(true, Ordering::Release);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn set_band_db(&self, index: usize, gain_db: f32) {
        if index >= BAND_COUNT {
            return;
        }
        let clamped = gain_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
        let stored = (clamped * 10.0).round() as i16;
        self.bands_db_x10[index].store(stored, Ordering::Release);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn read_bands_db(&self) -> [f32; BAND_COUNT] {
        let mut out = [0.0; BAND_COUNT];
        for (i, slot) in self.bands_db_x10.iter().enumerate() {
            out[i] = (slot.load(Ordering::Relaxed) as f32) / 10.0;
        }
        out
    }

    pub fn set_all_bands_db(&self, gains: &[f32]) {
        for (i, g) in gains.iter().take(BAND_COUNT).enumerate() {
            let clamped = g.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
            let stored = (clamped * 10.0).round() as i16;
            self.bands_db_x10[i].store(stored, Ordering::Release);
        }
        self.dirty.store(true, Ordering::Release);
    }
}

/// Per-decoder-thread filter chain. Owns the per-channel biquad
/// states and a cached coefficient array; coefficients are recomputed
/// from `EqShared` only when the dirty flag is observed.
pub struct EqProcessor {
    /// 6 sets of 5 normalised coefficients.
    coeffs: [[f32; 5]; BAND_COUNT],
    /// Per-channel × per-band state. Channels grow lazily — DSF's 8-
    /// channel files won't pay for state allocation when the rest of
    /// the library is stereo.
    states: Vec<[BiquadState; BAND_COUNT]>,
    /// Sample rate the cached coefficients were computed against.
    /// Tracked separately so a sample-rate change (track A 44.1 →
    /// track B 48) forces a recompute even when bands didn't move.
    cached_rate: f32,
    /// Linear gain applied ahead of the filters so the cascade's peak
    /// stays at or under unity; see the module's note on headroom. The
    /// gain the last buffer ended on.
    preamp: f32,
    /// The gain a new setting asks for. Lowered at once, raised over one
    /// buffer: a new boost acts from the very next sample, so the
    /// attenuation that keeps it under full scale has to as well, while
    /// giving that attenuation back can take a buffer, where a step of
    /// several dB between two samples would be heard as a click.
    preamp_target: f32,
}

impl EqProcessor {
    pub fn new() -> Self {
        Self {
            coeffs: [[1.0, 0.0, 0.0, 0.0, 0.0]; BAND_COUNT],
            states: Vec::new(),
            cached_rate: 0.0,
            preamp: 1.0,
            preamp_target: 1.0,
        }
    }

    /// Reset every per-channel state. Call after a seek, track switch,
    /// or any other discontinuity to avoid clicks from leftover memory.
    pub fn reset_states(&mut self) {
        for chan in self.states.iter_mut() {
            for st in chan.iter_mut() {
                st.reset();
            }
        }
    }

    /// Recompute coefficients from the shared band gains. Called when
    /// the dirty bit was observed OR when the sample rate doesn't
    /// match the cached one.
    fn recompute(&mut self, shared: &EqShared, sample_rate: f32) {
        let bands = shared.read_bands_db();
        for (i, &freq) in BAND_FREQS.iter().enumerate() {
            self.coeffs[i] = peaking_biquad(freq, sample_rate, bands[i], BAND_Q);
        }
        self.preamp_target = headroom_preamp(&self.coeffs, sample_rate);
        self.cached_rate = sample_rate;
    }

    /// Apply the filter chain in-place to an interleaved f32 buffer.
    /// No-op when the master bypass is off — single relaxed atomic
    /// load per call.
    pub fn process(
        &mut self,
        buf: &mut [f32],
        channels: usize,
        sample_rate: f32,
        shared: &EqShared,
    ) {
        if !shared.is_enabled() || channels == 0 || buf.is_empty() {
            return;
        }
        let dirty = shared.dirty.swap(false, Ordering::AcqRel);
        if dirty || self.cached_rate != sample_rate {
            self.recompute(shared, sample_rate);
        }
        // Grow per-channel state slots on demand.
        while self.states.len() < channels {
            self.states.push([BiquadState::default(); BAND_COUNT]);
        }
        let start = self.preamp.min(self.preamp_target);
        apply_preamp(buf, channels, start, self.preamp_target);
        self.preamp = self.preamp_target;
        let frames = buf.len() / channels;
        for f in 0..frames {
            for ch in 0..channels {
                let i = f * channels + ch;
                let mut x = buf[i];
                let chan_state = &mut self.states[ch];
                for (b, biquad) in chan_state.iter_mut().enumerate().take(BAND_COUNT) {
                    x = biquad.process(x, &self.coeffs[b]);
                }
                buf[i] = x;
            }
        }
    }
}

/// Built-in presets, gain in dB per band, tuned by ear against typical
/// tracks. Order matches `BAND_FREQS`.
pub const PRESETS: &[(&str, [f32; BAND_COUNT])] = &[
    ("flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("acoustic", [5.0, 4.5, 3.5, 1.0, 1.5, 3.0]),
    ("bass_booster", [6.0, 5.0, 3.0, 0.0, 0.0, 0.0]),
    ("bass_reducer", [-6.0, -5.0, -3.0, 0.0, 0.0, 0.0]),
    ("classical", [5.0, 4.0, -2.0, -2.0, 0.0, 3.0]),
    ("dance", [6.0, 5.0, 1.0, 2.0, 4.0, 5.0]),
    ("deep", [5.0, 3.0, 1.0, 2.0, -1.0, -3.0]),
    ("electronic", [4.0, 3.0, -2.0, 2.0, 1.0, 5.0]),
    ("hip_hop", [5.0, 4.0, 1.0, 1.0, -1.0, 2.0]),
    ("jazz", [4.0, 3.0, 1.0, 2.0, 3.0, 4.0]),
    ("latin", [5.0, 3.0, 0.0, 0.0, 3.0, 4.0]),
    ("loudness", [6.0, 3.0, 0.0, 3.0, 3.0, -2.0]),
    ("lounge", [-3.0, 1.0, 4.0, 5.0, 2.0, -1.0]),
    ("piano", [3.0, 2.0, 0.0, 3.0, 4.0, 3.0]),
    ("pop", [-1.0, 2.0, 5.0, 4.0, 1.0, -2.0]),
    ("rnb", [3.0, 5.0, 4.0, 1.0, 2.0, 3.0]),
    ("rock", [5.0, 4.0, 3.0, -1.0, 1.0, 5.0]),
    ("small_speakers", [5.0, 4.0, 3.0, 2.0, 1.0, 0.0]),
    ("spoken_word", [-3.0, -2.0, 0.0, 5.0, 4.0, 0.0]),
    ("treble_booster", [0.0, 0.0, 0.0, 3.0, 5.0, 6.0]),
];

/// Look up a preset by its lower-snake-case key.
pub fn preset_gains(key: &str) -> Option<[f32; BAND_COUNT]> {
    PRESETS.iter().find(|(k, _)| *k == key).map(|(_, g)| *g)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 44_100.0;

    fn chain(gains: [f32; BAND_COUNT]) -> [[f32; 5]; BAND_COUNT] {
        let mut coeffs = [[1.0, 0.0, 0.0, 0.0, 0.0]; BAND_COUNT];
        for (i, &freq) in BAND_FREQS.iter().enumerate() {
            coeffs[i] = peaking_biquad(freq, RATE, gains[i], BAND_Q);
        }
        coeffs
    }

    fn db(linear: f32) -> f32 {
        20.0 * linear.log10()
    }

    /// A new preamp is reached over the buffer, not between two samples.
    #[test]
    fn the_preamp_ramps_across_a_buffer() {
        let mut buf = vec![1.0_f32; 8]; // 4 stereo frames
        apply_preamp(&mut buf, 2, 1.0, 0.5);
        assert_eq!(buf, vec![0.875, 0.875, 0.75, 0.75, 0.625, 0.625, 0.5, 0.5]);

        let mut steady = vec![0.3_f32, -0.7, 0.1, 0.9];
        apply_preamp(&mut steady, 2, 1.0, 1.0);
        assert_eq!(steady, vec![0.3, -0.7, 0.1, 0.9]);
    }

    /// More attenuation is applied at once, less is given back over the
    /// next buffer.
    #[test]
    fn the_preamp_drops_at_once_and_recovers_over_a_buffer() {
        let shared = EqShared::new();
        shared.set_enabled(true);
        shared.set_band_db(3, 6.0);
        let mut eq = EqProcessor::new();
        let mut silence = vec![0.0_f32; 64];
        eq.process(&mut silence, 1, RATE, &shared);
        let boosted = eq.preamp;
        assert!(boosted < 0.5, "{boosted}");

        shared.set_band_db(3, 0.0);
        let mut buf = vec![1.0_f32; 4];
        eq.process(&mut buf, 1, RATE, &shared);
        // Flat again: the gain climbs back to 1.0 over the buffer, from
        // where it was, rather than jumping.
        assert!(buf[0] < 0.9 && buf[0] > boosted, "{buf:?}");
        assert!((buf[3] - 1.0).abs() < 1e-6, "{buf:?}");
        assert_eq!(eq.preamp, 1.0);
    }

    #[test]
    fn a_flat_or_cutting_setting_leaves_the_level_alone() {
        assert_eq!(headroom_preamp(&chain([0.0; BAND_COUNT]), RATE), 1.0);
        let cuts = preset_gains("bass_reducer").unwrap();
        assert_eq!(headroom_preamp(&chain(cuts), RATE), 1.0);
    }

    #[test]
    fn a_lone_boost_is_matched_by_the_preamp() {
        let mut gains = [0.0; BAND_COUNT];
        gains[3] = 6.0;
        // The boost, plus the 1 dB transient margin.
        let preamp = db(headroom_preamp(&chain(gains), RATE));
        assert!((preamp + 7.0).abs() < 0.1, "{preamp} dB");
    }

    #[test]
    fn overlapping_boosts_take_more_than_the_largest_slider() {
        // Bass booster: +6 / +5 / +3 dB on the three lowest bands, which
        // overlap; their sum peaks past the +6 dB slider.
        let gains = preset_gains("bass_booster").unwrap();
        let preamp = db(headroom_preamp(&chain(gains), RATE));
        assert!(preamp < -7.5, "{preamp} dB");
    }

    /// The worst transient measured: a sharp full-scale pattern through
    /// +12 dB at 15 kHz overshoots the response peak by 0.9 dB, which the
    /// margin absorbs.
    #[test]
    fn a_sharp_full_scale_pattern_under_a_treble_boost_stays_under_unity() {
        let shared = EqShared::new();
        shared.set_enabled(true);
        shared.set_band_db(BAND_COUNT - 1, MAX_GAIN_DB);
        let mut eq = EqProcessor::new();
        let mut buf: Vec<f32> = [1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 1.0].repeat(3);
        eq.process(&mut buf, 1, RATE, &shared);
        let peak = buf.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!(peak <= 1.0, "peak {peak}");
    }

    #[test]
    fn a_full_scale_tone_under_full_boost_stays_under_unity() {
        let shared = EqShared::new();
        shared.set_enabled(true);
        shared.set_band_db(0, MAX_GAIN_DB);
        let mut eq = EqProcessor::new();
        // One second of a full-scale 60 Hz sine, mono.
        let mut buf: Vec<f32> = (0..RATE as usize)
            .map(|n| (2.0 * PI * 60.0 * n as f32 / RATE).sin())
            .collect();
        eq.process(&mut buf, 1, RATE, &shared);
        // Past the filter's settling time, the output peaks just under
        // full scale (the 1 dB margin) instead of +12 dB over it.
        let settled = &buf[buf.len() / 2..];
        let peak = settled.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!(peak <= 0.9 && peak > 0.85, "peak {peak}");
    }
}
