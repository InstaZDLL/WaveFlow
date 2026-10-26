//! The musical key, from the same decimated mono signal as the tempo.
//!
//! The classic method, in two steps:
//!
//! - **A chromagram**: how much of each of the twelve pitch classes the
//!   track holds, summed over the whole track. Each frame's spectrum is
//!   **whitened** — every bin less the mean of the bins within about two
//!   semitones of it — and only the **peaks** of what remains are folded
//!   onto the twelve semitones, each towards the one it is nearest,
//!   weighted by how near. Whitening is what takes the drums out: a kick
//!   or a hi-hat is broadband, it lifts a whole region evenly, and the
//!   local mean cancels it, where a note is a narrow peak that stands
//!   above it. Each frame is then scaled by its own loudest class, so a
//!   loud chorus weighs no more than a quiet verse.
//! - **Key profiles**: how much each degree of the scale is expected to
//!   sound in a major and a minor key. The profile, rotated to each of
//!   the twelve tonics, is correlated with the chromagram, and the best
//!   of the 24 is the key.
//!
//! The frame is long — 4096 samples at the ~11 kHz decimated rate, so
//! 2.7 Hz per bin — because a semitone is only 3 Hz wide in the bass;
//! the tempo's 1024-sample frames would smear a bass line across three
//! notes. Frames are 2048 samples apart, about five a second, which is
//! plenty for a measure summed over minutes.
//!
//! Every choice here was measured on the GiantSteps Key dataset (604
//! hand-corrected previews); see the docs for the numbers, including
//! the variants that lost — compressing the magnitudes (square root or
//! log) cost up to 40 points, and folding harmonics onto their
//! fundamentals (HPCP) gained nothing.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

const FRAME: usize = 4096;
const HOP: usize = 2048;
/// The band folded onto pitch classes. Above ~1 kHz the harmonics of
/// the notes below blur the picture more than the notes there add to it;
/// below 40 Hz the bins are wider than a semitone.
const MIN_HZ: f64 = 40.0;
const MAX_HZ: f64 = 1_000.0;
/// Half-width of the whitening mean, as a share of the bin's own
/// frequency: 0.12 is about two semitones each side.
const WHITEN_SPAN: f64 = 0.12;
/// A frame whose loudest class is below this is silence, and adds
/// nothing rather than noise.
const SILENT: f32 = 1e-4;
/// Audible frames below which there is no key to read (~6 seconds).
const MIN_FRAMES: u32 = 30;

/// Names of the 24 keys in the form WaveFlow stores them, by tonic pitch
/// class from C. The spelling is the one a key signature would use
/// (E♭ major, but F♯ minor), with plain ASCII for the accidentals, so a
/// stored value reads like a tag a DJ tool writes.
pub const MAJOR_NAMES: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];
pub const MINOR_NAMES: [&str; 12] = [
    "Cm", "C#m", "Dm", "Ebm", "Em", "Fm", "F#m", "Gm", "G#m", "Am", "Bbm", "Bm",
];

/// The key a track shows, as a SQL expression over the `track t` row:
/// its tag's when it has one, the analysis's otherwise.
///
/// A tag wins because the tools that write one (DJ software, mostly)
/// read keys better than this module does — it is right on about half of
/// a hand-annotated set — and a user who tagged a key chose it. Kept
/// apart from `track.musical_key` rather than written into it, because a
/// rescan rewrites that column from the file and would erase a value the
/// tag never held.
///
/// A macro rather than a `const` so that every query using it stays a
/// `&'static str` built by `concat!`; five queries read the key, and a
/// copy that drifted would show one key in the list and another in the
/// properties.
#[macro_export]
macro_rules! effective_musical_key_sql {
    () => {
        "COALESCE(NULLIF(TRIM(t.musical_key), ''), \
         (SELECT tk.musical_key FROM track_analysis tk WHERE tk.track_id = t.id))"
    };
}

/// A key profile: the expected weight of each scale degree from the
/// tonic, for major and for minor.
pub struct Profile {
    pub major: [f64; 12],
    pub minor: [f64; 12],
}

/// Shaath's profile (KeyFinder, 2011): Krumhansl & Kessler's listener
/// ratings, adjusted on popular and dance music. It led every other
/// profile measured here (Krumhansl, Temperley, Faraldo's EDMA) on every
/// chromagram variant.
pub const PROFILE: Profile = Profile {
    major: [6.6, 2.0, 3.5, 2.3, 4.6, 4.0, 2.5, 5.2, 2.4, 3.7, 2.3, 3.4],
    minor: [6.5, 2.7, 3.5, 5.4, 2.6, 3.5, 2.5, 5.2, 4.0, 2.7, 4.3, 3.2],
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    /// Pitch class of the tonic, 0 = C.
    pub tonic: u8,
    pub minor: bool,
    /// 0 to 1: how far the best key stands out from the next one.
    pub confidence: f64,
}

impl Key {
    /// The stored spelling, as [`MAJOR_NAMES`] / [`MINOR_NAMES`].
    pub fn name(&self) -> &'static str {
        if self.minor {
            MINOR_NAMES[self.tonic as usize % 12]
        } else {
            MAJOR_NAMES[self.tonic as usize % 12]
        }
    }
}

/// Streaming chromagram over the decimated mono signal.
pub(crate) struct KeyDetector {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    frame_in: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    pending: Vec<f32>,
    start: usize,
    /// For each bin in the band: the bin, its pitch class, its weight,
    /// and the half-width of its whitening mean.
    bins: Vec<(usize, usize, f32, usize)>,
    /// Magnitudes from bin `lo` on, their prefix sums, and the whitened
    /// band: buffers kept between frames.
    lo: usize,
    mags: Vec<f32>,
    prefix: Vec<f32>,
    white: Vec<f32>,
    chroma: [f64; 12],
    frames: u32,
}

impl KeyDetector {
    /// `rate` is the decimated sample rate, in Hz.
    pub(crate) fn new(rate: f64) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FRAME);
        let spectrum = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let window = (0..FRAME)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * i as f32 / FRAME as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        let bin_hz = rate / FRAME as f64;
        let last_bin = spectrum.len() - 1;
        let bins: Vec<(usize, usize, f32, usize)> = (2..last_bin - 1)
            .filter_map(|bin| {
                let hz = bin as f64 * bin_hz;
                if !(MIN_HZ..=MAX_HZ).contains(&hz) {
                    return None;
                }
                // MIDI numbering: 69 is A4 at 440 Hz, and C is a multiple
                // of 12, so `round(pitch) % 12` is the class from C.
                let pitch = 69.0 + 12.0 * (hz / 440.0).log2();
                let nearest = pitch.round();
                let weight = (std::f64::consts::PI * (pitch - nearest)).cos().powi(2) as f32;
                let half = ((bin as f64 * WHITEN_SPAN) as usize).max(2);
                Some((bin, (nearest as i64).rem_euclid(12) as usize, weight, half))
            })
            .collect();
        // The whitened band runs one bin past each end, for the peak
        // test, and the means reach `half` further still.
        let (lo, hi) = match (bins.first(), bins.last()) {
            (Some(f), Some(l)) => (f.0.saturating_sub(f.3 + 2), (l.0 + l.3 + 2).min(last_bin)),
            _ => (0, 0),
        };
        Self {
            fft,
            window,
            frame_in: vec![0.0; FRAME],
            spectrum,
            scratch,
            pending: Vec::with_capacity(FRAME * 4),
            start: 0,
            bins,
            lo,
            mags: vec![0.0; hi - lo + 1],
            prefix: vec![0.0; hi - lo + 2],
            white: vec![0.0; hi - lo + 1],
            chroma: [0.0; 12],
            frames: 0,
        }
    }

    pub(crate) fn push(&mut self, sample: f32) {
        self.pending.push(sample);
        if self.pending.len() - self.start >= FRAME {
            self.process_frame();
            self.start += HOP;
            if self.start >= FRAME * 2 {
                self.pending.drain(..self.start);
                self.start = 0;
            }
        }
    }

    fn process_frame(&mut self) {
        if self.bins.is_empty() {
            return;
        }
        let frame = &self.pending[self.start..self.start + FRAME];
        for ((dst, &src), &w) in self.frame_in.iter_mut().zip(frame).zip(&self.window) {
            *dst = src * w;
        }
        if self
            .fft
            .process_with_scratch(&mut self.frame_in, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return;
        }

        let lo = self.lo;
        for (i, m) in self.mags.iter_mut().enumerate() {
            *m = self.spectrum[lo + i].norm();
        }
        self.prefix[0] = 0.0;
        for i in 0..self.mags.len() {
            self.prefix[i + 1] = self.prefix[i] + self.mags[i];
        }
        let len = self.mags.len();
        // Whiten every bin the peak test will look at: the band and one
        // bin either side.
        let first = self.bins[0].0 - 1 - lo;
        let last = self.bins[self.bins.len() - 1].0 + 1 - lo;
        for i in first..=last {
            let half = (((i + lo) as f64 * WHITEN_SPAN) as usize).max(2);
            let a = i.saturating_sub(half);
            let z = (i + half).min(len - 1);
            let mean = (self.prefix[z + 1] - self.prefix[a]) / (z - a + 1) as f32;
            self.white[i] = (self.mags[i] - mean).max(0.0);
        }

        let mut frame_chroma = [0.0f32; 12];
        for &(bin, class, weight, _) in &self.bins {
            let i = bin - lo;
            let v = self.white[i];
            if v > self.white[i - 1] && v >= self.white[i + 1] {
                frame_chroma[class] += weight * v;
            }
        }
        let peak = frame_chroma.iter().copied().fold(0.0f32, f32::max);
        if peak < SILENT {
            return;
        }
        for (sum, v) in self.chroma.iter_mut().zip(frame_chroma) {
            *sum += f64::from(v / peak);
        }
        self.frames += 1;
    }

    /// The summed chromagram, scaled to sum to 1, or `None` for a track
    /// with too little to read a key from.
    pub(crate) fn chroma(&self) -> Option<[f64; 12]> {
        if self.frames < MIN_FRAMES {
            return None;
        }
        let total: f64 = self.chroma.iter().sum();
        if total <= 0.0 {
            return None;
        }
        let mut out = self.chroma;
        for v in out.iter_mut() {
            *v /= total;
        }
        Some(out)
    }

    pub(crate) fn finish(self) -> (Option<Key>, Option<[f64; 12]>) {
        let chroma = self.chroma();
        (chroma.and_then(|c| estimate_key(&c, &PROFILE)), chroma)
    }
}

fn pearson(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        return 0.0;
    }
    num / (da.sqrt() * db.sqrt())
}

/// The profile rotated so its tonic sits on pitch class `tonic`.
fn rotated(profile: &[f64; 12], tonic: usize) -> [f64; 12] {
    let mut out = [0.0; 12];
    for (degree, &w) in profile.iter().enumerate() {
        out[(tonic + degree) % 12] = w;
    }
    out
}

/// The best of the 24 keys for a chromagram, with a confidence from how
/// far it leads the runner-up.
pub fn estimate_key(chroma: &[f64; 12], profile: &Profile) -> Option<Key> {
    let mut scores: Vec<(f64, u8, bool)> = Vec::with_capacity(24);
    for tonic in 0..12 {
        scores.push((
            pearson(chroma, &rotated(&profile.major, tonic)),
            tonic as u8,
            false,
        ));
        scores.push((
            pearson(chroma, &rotated(&profile.minor, tonic)),
            tonic as u8,
            true,
        ));
    }
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (best, tonic, minor) = scores[0];
    if best <= 0.0 {
        return None;
    }
    let runner_up = scores[1].0.max(0.0);
    let confidence = ((best - runner_up) / best * CONFIDENCE_SCALE).clamp(0.0, 1.0);
    Some(Key {
        tonic,
        minor,
        confidence,
    })
}

/// Turns the winner's lead over the runner-up, as a share of its own
/// correlation, into 0..1. The runner-up is often a close relative —
/// the relative or parallel key, or a fifth away — so leads are small
/// by nature; see the docs for how the scale was set.
const CONFIDENCE_SCALE: f64 = 4.0;

#[cfg(test)]
mod tests;
