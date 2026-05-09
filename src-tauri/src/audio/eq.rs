//! 6-band peaking-EQ DSP, Spotify-style.
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

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicI16, Ordering};

/// Centre frequencies for each band, picked to match Spotify so the
/// visual UI maps 1:1 with what users may already be familiar with.
pub const BAND_FREQS: [f32; 6] = [60.0, 150.0, 400.0, 1000.0, 2400.0, 15000.0];

/// Number of bands. Pulled into a const so the array sizes stay in
/// sync everywhere.
pub const BAND_COUNT: usize = BAND_FREQS.len();

/// Q factor for every band. ~1/sqrt(2) gives ~1-octave bandwidth at
/// the centre frequency — wide enough that adjacent bands overlap
/// naturally, narrow enough that cuts/boosts feel surgical.
const BAND_Q: f32 = 0.707;

/// Maximum cap (absolute value) for each band's gain in dB. Mirrors
/// Spotify's ±12 dB. The atomic stores tenths of dB (i16 range fits
/// up to ±3276 dB so we have plenty of head-room).
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
}

impl EqProcessor {
    pub fn new() -> Self {
        Self {
            coeffs: [[1.0, 0.0, 0.0, 0.0, 0.0]; BAND_COUNT],
            states: Vec::new(),
            cached_rate: 0.0,
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
        let frames = buf.len() / channels;
        for f in 0..frames {
            for ch in 0..channels {
                let i = f * channels + ch;
                let mut x = buf[i];
                let chan_state = &mut self.states[ch];
                for b in 0..BAND_COUNT {
                    x = chan_state[b].process(x, &self.coeffs[b]);
                }
                buf[i] = x;
            }
        }
    }
}

/// Built-in presets, gain in dB per band. Calibrated to feel close to
/// Spotify's own selections without claiming to be byte-identical
/// (Spotify doesn't publish the exact values — these are tuned by ear
/// against typical tracks). Order matches `BAND_FREQS`.
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
