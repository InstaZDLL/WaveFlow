//! Onsets, tempo and energy, from one pass over a decimated mono signal.
//!
//! Version 1 of the tempo estimator auto-correlated an RMS envelope over
//! 60-200 BPM and took the strongest lag. Two of its failures reached
//! the user (#848): it read fast tracks at half speed, and when nothing
//! stood out it stuck to the range edge, so "60 BPM" meant "no idea" as
//! often as it meant 60. Version 2 changes the three stages:
//!
//! - **Onsets** are a spectral flux: frame by frame, how much each
//!   frequency band rose, in dB. An RMS envelope only sees the sum, so a
//!   hi-hat over a held chord barely moves it; band by band it is a
//!   clear attack. Working in dB also makes the flux blind to the track's
//!   overall level: a gain change cancels out of every difference, which
//!   is what lets [`Energy`] survive loudness normalisation.
//! - **Tempo** is chosen over 40-240 BPM, from an autocorrelation
//!   averaged over 8-second windows, scored with its first harmonic and
//!   a prior that leans towards common tempos. See [`estimate_tempo`].
//! - **Octave ties** — the autocorrelation repeats as well at twice a
//!   beat's period as at the period itself, and the prior is symmetric
//!   in octaves, so 85 and 170 can score alike. When they do, the
//!   track's [`Energy`] decides: an intense track takes the faster
//!   reading, a calm one the slower, which is how a listener would tap
//!   it and what a mood needs.
//! - **Confidence** says how far to trust it, from 0 to 1: how clearly
//!   the winner stands out, how sure the octave is, and how steady the
//!   tempo is from window to window. A winner sitting on the range edge
//!   gets almost none, which is the honest reading of "no idea".

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Samples per analysis frame at the decimated rate (~93 ms at 11 kHz):
/// long enough to resolve a bass note, short enough to separate attacks.
const FRAME: usize = 1024;
/// Samples between frames (~11.6 ms at 11 kHz, ~86 frames per second):
/// twice the resolution of version 1's envelope.
const HOP: usize = 128;
/// Bands above this are left out of the flux: the decimated signal has
/// little above it, and what is there is mostly aliasing.
const MAX_FLUX_HZ: f64 = 5_000.0;
/// Floor for the dB magnitudes, relative to a full-scale sine. Without
/// it, silent bins would swing by tens of dB on rounding noise.
const FLOOR: f32 = 1e-5;

/// The tempo range searched. Wider than version 1's 60-200 so that a
/// slow ballad or a 200+ track can be read as what it is rather than
/// folded onto the edge.
pub const MIN_BPM: f64 = 40.0;
pub const MAX_BPM: f64 = 240.0;
/// Centre of the tempo prior. Most music sits between 80 and 160 BPM;
/// the prior only breaks near-ties between metrical levels, it does not
/// pull a clear reading towards 120.
const PRIOR_CENTRE_BPM: f64 = 120.0;
/// Width of the prior, in octaves.
const PRIOR_OCTAVES: f64 = 1.0;
/// Weight of the first harmonic (twice the period) in a candidate's
/// score: a real beat repeats at every multiple of its period.
const HARMONIC_WEIGHT: f32 = 0.5;
/// Local windows for the averaged autocorrelation and the stability
/// measure, in seconds.
const WINDOW_SECS: f64 = 8.0;
const WINDOW_HOP_SECS: f64 = 2.0;
/// Shortest stretch worth a tempo, in seconds.
const MIN_SECS: f64 = 6.0;
/// Two tempos this close (relative) are the same reading.
const SAME_TEMPO: f64 = 0.05;
/// An octave rival scoring at least this share of the winner is a tie,
/// settled by the energy.
const OCTAVE_TIE: f64 = 0.9;
/// Energy at or above which a tie goes to the faster octave.
const FAST_ENERGY: f64 = 0.5;

/// Streaming onset detector over the decimated mono signal.
pub(crate) struct OnsetDetector {
    rate: f64,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    frame_in: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Samples not yet consumed by a frame; `start` is the first one
    /// the next frame reads, so the buffer is compacted only now and then.
    pending: Vec<f32>,
    start: usize,
    /// Highest bin in the flux.
    bins: usize,
    prev_db: Vec<f32>,
    has_prev: bool,
    /// One flux value per frame, in dB per band.
    flux: Vec<f32>,
    /// Per-frame level in dB, for the dynamics part of [`Energy`].
    level_db: Vec<f32>,
    /// Running sums for the spectral centroid of [`Energy`].
    centroid_sum: f64,
    centroid_frames: u64,
}

impl OnsetDetector {
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
        let bins = ((MAX_FLUX_HZ / bin_hz).floor() as usize).clamp(1, FRAME / 2);
        Self {
            rate,
            fft,
            window,
            frame_in: vec![0.0; FRAME],
            spectrum,
            scratch,
            pending: Vec::with_capacity(FRAME * 8),
            start: 0,
            bins,
            prev_db: vec![0.0; bins],
            has_prev: false,
            flux: Vec::new(),
            level_db: Vec::new(),
            centroid_sum: 0.0,
            centroid_frames: 0,
        }
    }

    pub(crate) fn push(&mut self, sample: f32) {
        self.pending.push(sample);
        if self.pending.len() - self.start >= FRAME {
            self.process_frame();
            self.start += HOP;
            if self.start >= FRAME * 4 {
                self.pending.drain(..self.start);
                self.start = 0;
            }
        }
    }

    fn process_frame(&mut self) {
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

        // A full-scale sine through a Hann window peaks at FRAME / 4.
        let norm = 4.0 / FRAME as f32;
        let mut rise = 0.0f32;
        let mut power = 0.0f64;
        let mut weighted = 0.0f64;
        let bin_hz = self.rate / FRAME as f64;
        // Bin 0 is DC: no attack lives there.
        for bin in 1..=self.bins.min(self.spectrum.len() - 1) {
            let magnitude = self.spectrum[bin].norm() * norm;
            let db = 20.0 * (magnitude + FLOOR).log10();
            let slot = bin - 1;
            if slot < self.prev_db.len() {
                if self.has_prev {
                    rise += (db - self.prev_db[slot]).max(0.0);
                }
                self.prev_db[slot] = db;
            }
            let p = f64::from(magnitude) * f64::from(magnitude);
            power += p;
            weighted += p * bin as f64 * bin_hz;
        }
        let level = (power / self.bins as f64).max(1e-12);
        // The first frame only seeds `prev_db`: it has no rise to report,
        // and its level is left out with it so `flux[i]` and
        // `level_db[i]` keep describing the same frame.
        if self.has_prev {
            self.flux.push(rise / self.bins as f32);
            self.level_db.push(10.0 * level.log10() as f32);
        }
        self.has_prev = true;
        // The centroid of a silent frame is noise; only frames with
        // something in them count.
        if level > 1e-8 {
            self.centroid_sum += weighted / power;
            self.centroid_frames += 1;
        }
    }

    /// Frames per second of the flux.
    pub(crate) fn frames_per_sec(&self) -> f64 {
        self.rate / HOP as f64
    }

    pub(crate) fn finish(self) -> Rhythm {
        let fps = self.frames_per_sec();
        let onset = onset_envelope(&self.flux, fps);
        let energy = energy(&self.flux, &onset, &self.level_db, fps, {
            if self.centroid_frames > 0 {
                Some(self.centroid_sum / self.centroid_frames as f64)
            } else {
                None
            }
        });
        let tempo = estimate_tempo(&onset, fps, energy.map(|e| e.value));
        Rhythm { tempo, energy }
    }
}

/// What [`OnsetDetector::finish`] measured.
#[derive(Debug, Clone, Default)]
pub struct Rhythm {
    pub tempo: Option<Tempo>,
    pub energy: Option<Energy>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tempo {
    pub bpm: f64,
    /// 0 to 1. See the module documentation.
    pub confidence: f64,
    /// The parts of `confidence`, kept for tuning.
    pub salience: f64,
    pub octave_margin: f64,
    pub stability: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Energy {
    /// 0 (calm) to 1 (intense).
    pub value: f64,
    /// The parts of `value`, kept for tuning.
    pub attack_db: f64,
    pub onset_rate: f64,
    pub centroid_hz: f64,
    pub dynamics_db: f64,
}

/// The flux with its slow trend removed and its dips cut off, so what
/// is left is the attacks alone.
fn onset_envelope(flux: &[f32], fps: f64) -> Vec<f32> {
    let half = ((0.2 * fps).round() as usize).max(1);
    let mut prefix = Vec::with_capacity(flux.len() + 1);
    prefix.push(0.0f64);
    for &v in flux {
        prefix.push(prefix.last().unwrap() + f64::from(v));
    }
    (0..flux.len())
        .map(|i| {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(flux.len());
            let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f64;
            (f64::from(flux[i]) - mean).max(0.0) as f32
        })
        .collect()
}

fn lag_to_bpm(lag: f64, fps: f64) -> f64 {
    60.0 * fps / lag
}

fn bpm_to_lag(bpm: f64, fps: f64) -> f64 {
    60.0 * fps / bpm
}

/// Autocorrelation of `x` for lags `0..=max_lag`, each lag averaged over
/// its own number of pairs and the whole scaled by lag 0, so a loud
/// window weighs no more than a quiet one.
fn autocorrelation(x: &[f32], max_lag: usize) -> Option<Vec<f32>> {
    let max_lag = max_lag.min(x.len().saturating_sub(1));
    let mut acf = vec![0.0f32; max_lag + 1];
    for (lag, slot) in acf.iter_mut().enumerate() {
        let n = x.len() - lag;
        let mut sum = 0.0f32;
        for i in 0..n {
            sum += x[i] * x[i + lag];
        }
        *slot = sum / n as f32;
    }
    let zero = acf[0];
    if zero <= f32::EPSILON {
        return None;
    }
    for v in acf.iter_mut() {
        *v /= zero;
    }
    Some(acf)
}

/// Linear interpolation into `acf` at a fractional lag.
fn at(acf: &[f32], lag: f64) -> f32 {
    if lag < 0.0 {
        return 0.0;
    }
    let i = lag.floor() as usize;
    if i + 1 >= acf.len() {
        return 0.0;
    }
    let t = (lag - i as f64) as f32;
    acf[i] * (1.0 - t) + acf[i + 1] * t
}

fn prior(bpm: f64) -> f64 {
    let octaves = (bpm / PRIOR_CENTRE_BPM).log2() / PRIOR_OCTAVES;
    (-0.5 * octaves * octaves).exp()
}

/// A candidate's score at a fractional lag: its own correlation and its
/// first harmonic's, weighted by the prior.
fn score(acf: &[f32], lag: f64, fps: f64) -> f64 {
    let raw = at(acf, lag).max(0.0) + HARMONIC_WEIGHT * at(acf, 2.0 * lag).max(0.0);
    f64::from(raw) * prior(lag_to_bpm(lag, fps))
}

/// The best lag in the search range for one autocorrelation, refined
/// between integer lags, with every integer lag's score alongside.
fn best_lag(acf: &[f32], fps: f64) -> Option<(f64, Vec<(f64, f64)>)> {
    let min_lag = bpm_to_lag(MAX_BPM, fps).floor().max(1.0) as usize;
    let max_lag = bpm_to_lag(MIN_BPM, fps).ceil() as usize;
    if 2 * max_lag >= acf.len() {
        return None;
    }
    let scores: Vec<(f64, f64)> = (min_lag..=max_lag)
        .map(|lag| (lag as f64, score(acf, lag as f64, fps)))
        .collect();
    let (best_i, &(best, best_score)) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1 .1.total_cmp(&b.1 .1))?;
    if best_score <= 0.0 {
        return None;
    }
    // Parabolic refinement over the two neighbours.
    let refined = if best_i > 0 && best_i + 1 < scores.len() {
        let (l, c, r) = (scores[best_i - 1].1, best_score, scores[best_i + 1].1);
        let denom = l - 2.0 * c + r;
        if denom.abs() > f64::EPSILON {
            best + (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
        } else {
            best
        }
    } else {
        best
    };
    Some((refined, scores))
}

/// Whether two tempos are the same reading, or one is the other's
/// double, half, or the 3:2 / 2:3 a triplet feel produces.
fn related(a: f64, b: f64) -> bool {
    [1.0, 2.0, 0.5, 1.5, 2.0 / 3.0]
        .iter()
        .any(|ratio| ((a * ratio) / b - 1.0).abs() < SAME_TEMPO)
}

fn same_octave_family(a: f64, b: f64) -> bool {
    [1.0, 2.0, 0.5]
        .iter()
        .any(|ratio| ((a * ratio) / b - 1.0).abs() < SAME_TEMPO)
}

/// Pick a tempo from the onset envelope. See the module documentation;
/// `energy` settles octave ties, and without it the prior's pick stands.
pub(crate) fn estimate_tempo(onset: &[f32], fps: f64, energy: Option<f64>) -> Option<Tempo> {
    if fps <= 0.0 || (onset.len() as f64) < MIN_SECS * fps {
        return None;
    }
    let max_lag = (2.0 * bpm_to_lag(MIN_BPM, fps)).ceil() as usize + 1;
    let window = ((WINDOW_SECS * fps).round() as usize).max(max_lag + 1);
    let hop = ((WINDOW_HOP_SECS * fps).round() as usize).max(1);

    // Average the windows' autocorrelations: a tempo that holds across
    // the track adds up, a fill or a break does not.
    let mut windows: Vec<Vec<f32>> = Vec::new();
    if onset.len() <= window {
        windows.extend(autocorrelation(onset, max_lag));
    } else {
        let mut start = 0;
        while start + window <= onset.len() {
            windows.extend(autocorrelation(&onset[start..start + window], max_lag));
            start += hop;
        }
    }
    if windows.is_empty() {
        return None;
    }
    let len = windows.iter().map(Vec::len).min()?;
    let mut mean = vec![0.0f32; len];
    for acf in &windows {
        for (m, v) in mean.iter_mut().zip(acf) {
            *m += v;
        }
    }
    for m in mean.iter_mut() {
        *m /= windows.len() as f32;
    }

    let (lag, scores) = best_lag(&mean, fps)?;
    let mut bpm = lag_to_bpm(lag, fps);
    let mut best = score(&mean, lag, fps).max(f64::EPSILON);

    // Octave tie: let the energy choose between the winner and whichever
    // of its double or half comes closest.
    if let Some(energy) = energy {
        let rival = [2.0, 0.5]
            .iter()
            .map(|ratio| bpm * ratio)
            .filter(|b| (MIN_BPM..=MAX_BPM).contains(b))
            .map(|b| (b, score(&mean, bpm_to_lag(b, fps), fps)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((rival_bpm, rival_score)) = rival {
            let wants_faster = energy >= FAST_ENERGY;
            if rival_score >= OCTAVE_TIE * best && (rival_bpm > bpm) == wants_faster {
                // Refine the rival the way the winner was refined.
                let (l, c, r) = (
                    score(&mean, bpm_to_lag(rival_bpm, fps) - 1.0, fps),
                    rival_score,
                    score(&mean, bpm_to_lag(rival_bpm, fps) + 1.0, fps),
                );
                let denom = l - 2.0 * c + r;
                let shift = if denom.abs() > f64::EPSILON {
                    (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                let rival_lag = bpm_to_lag(rival_bpm, fps) + shift;
                bpm = lag_to_bpm(rival_lag, fps);
                best = score(&mean, rival_lag, fps)
                    .max(rival_score)
                    .max(f64::EPSILON);
            }
        }
    }

    // Salience: the best unrelated candidate against the winner.
    let rival = scores
        .iter()
        .filter(|(l, _)| !related(lag_to_bpm(*l, fps), bpm))
        .map(|&(_, s)| s)
        .fold(0.0f64, f64::max);
    let salience = (1.0 - rival / best).clamp(0.0, 1.0);

    // Octave margin: how far ahead of its own double and half it is.
    let octave_rival = [2.0, 0.5]
        .iter()
        .map(|ratio| bpm * ratio)
        .filter(|b| (MIN_BPM..=MAX_BPM).contains(b))
        .map(|b| score(&mean, bpm_to_lag(b, fps), fps))
        .fold(0.0f64, f64::max);
    let octave_margin = (1.0 - octave_rival / best).clamp(0.0, 1.0);

    // Stability: the share of windows that agree, octave aside.
    let agreeing = windows
        .iter()
        .filter_map(|acf| best_lag(acf, fps))
        .filter(|(l, _)| same_octave_family(lag_to_bpm(*l, fps), bpm))
        .count();
    let stability = agreeing as f64 / windows.len() as f64;

    // A winner on the range edge is the search running out, not a tempo.
    let edge = (bpm / MIN_BPM - 1.0).abs() < 0.03 || (bpm / MAX_BPM - 1.0).abs() < 0.03;
    let confidence = if edge {
        0.0
    } else {
        (stability * (2.0 * salience).min(1.0) * (0.5 + octave_margin).min(1.0)).clamp(0.0, 1.0)
    };

    Some(Tempo {
        bpm,
        confidence,
        salience,
        octave_margin,
        stability,
    })
}

/// How intense the track is, from features that a loudness
/// normalisation leaves alone. None of them is a level: the attacks are
/// differences in dB, the onset rate is a count, the centroid is a
/// frequency and the dynamics are a spread in dB.
fn energy(
    flux: &[f32],
    onset: &[f32],
    level_db: &[f32],
    fps: f64,
    centroid_hz: Option<f64>,
) -> Option<Energy> {
    if (flux.len() as f64) < MIN_SECS * fps {
        return None;
    }
    // Only frames with sound in them: a fade or a silent gap is not calm
    // music, it is no music.
    let loudest = level_db.iter().copied().fold(f32::MIN, f32::max);
    let audible: Vec<usize> = (0..flux.len().min(level_db.len()))
        .filter(|&i| level_db[i] > loudest - 50.0)
        .collect();
    if audible.len() < 2 {
        return None;
    }

    let attack_db = audible.iter().map(|&i| f64::from(flux[i])).sum::<f64>() / audible.len() as f64;

    // Attacks per second: local maxima of the onset envelope that stand
    // out from its typical level.
    let mut sorted: Vec<f32> = onset.to_vec();
    sorted.sort_by(f32::total_cmp);
    let threshold = sorted[sorted.len() * 3 / 4].max(f32::EPSILON) * 1.5;
    let peaks = (1..onset.len().saturating_sub(1))
        .filter(|&i| onset[i] > threshold && onset[i] >= onset[i - 1] && onset[i] > onset[i + 1])
        .count();
    let onset_rate = peaks as f64 / (onset.len() as f64 / fps);

    // Spread of the frame levels: a quiet piece breathes, a dense mix
    // stays pinned.
    let levels: Vec<f64> = audible.iter().map(|&i| f64::from(level_db[i])).collect();
    let mean = levels.iter().sum::<f64>() / levels.len() as f64;
    let dynamics_db =
        (levels.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / levels.len() as f64).sqrt();

    let centroid_hz = centroid_hz.unwrap_or(0.0);

    // Each part mapped onto 0..1 around a midpoint, then weighted.
    let squash = |x: f64, mid: f64, width: f64| 1.0 / (1.0 + (-(x - mid) / width).exp());
    let blend = 0.45 * squash(attack_db, ATTACK_MID_DB, ATTACK_WIDTH_DB)
        + 0.25 * squash(onset_rate, RATE_MID, RATE_WIDTH)
        + 0.20 * squash(centroid_hz, CENTROID_MID_HZ, CENTROID_WIDTH_HZ)
        + 0.10 * (1.0 - squash(dynamics_db, DYNAMICS_MID_DB, DYNAMICS_WIDTH_DB));
    // A weighted mean of squashed parts bunches up around the middle (80%
    // of a real library fell between 0.30 and 0.64); stretch it back over
    // the scale so a threshold has room to mean something.
    let value = squash(blend, 0.5, BLEND_WIDTH);

    Some(Energy {
        value: value.clamp(0.0, 1.0),
        attack_db,
        onset_rate,
        centroid_hz,
        dynamics_db,
    })
}

// Midpoints and widths of the energy parts, from the `analysis_bench`
// example over a real 2,864-file library of mixed genres: each midpoint is
// the part's median, and each width puts its 10th and 90th percentiles
// near 0.15 and 0.85.
const ATTACK_MID_DB: f64 = 1.65;
const ATTACK_WIDTH_DB: f64 = 0.076;
const RATE_MID: f64 = 7.0;
const RATE_WIDTH: f64 = 0.45;
const CENTROID_MID_HZ: f64 = 564.0;
const CENTROID_WIDTH_HZ: f64 = 140.0;
const DYNAMICS_MID_DB: f64 = 5.7;
const DYNAMICS_WIDTH_DB: f64 = 1.14;
const BLEND_WIDTH: f64 = 0.1;

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 11_025.0;

    /// A click track: a short decaying noise burst on every beat, with an
    /// optional softer one halfway between.
    fn clicks(bpm: f64, secs: f64, offbeat: f32) -> Vec<f32> {
        let n = (secs * RATE) as usize;
        let period = 60.0 / bpm * RATE;
        let mut out = vec![0.0f32; n];
        let mut seed = 0x2545_f491_u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let burst = (0.03 * RATE) as usize;
        let mut beat = 0.0;
        let mut k = 0;
        while (beat as usize) < n {
            for (j, gain) in [(0.0, 0.8f32), (0.5, offbeat)] {
                if gain <= 0.0 {
                    continue;
                }
                let at = (beat + j * period) as usize;
                for i in 0..burst {
                    if at + i < n {
                        let decay = (-(i as f32) / (burst as f32 / 5.0)).exp();
                        out[at + i] += gain * decay * noise();
                    }
                }
            }
            k += 1;
            beat = k as f64 * period;
        }
        out
    }

    fn analyse(signal: &[f32]) -> Rhythm {
        let mut detector = OnsetDetector::new(RATE);
        for &s in signal {
            detector.push(s);
        }
        detector.finish()
    }

    fn tempo_of(signal: &[f32]) -> Tempo {
        analyse(signal).tempo.expect("a tempo")
    }

    #[test]
    fn reads_common_tempos_within_a_bpm() {
        for bpm in [70.0, 90.0, 120.0, 128.0, 140.0, 174.0] {
            let t = tempo_of(&clicks(bpm, 40.0, 0.0));
            // A plain click track has no octave to prefer beyond the
            // prior; what must hold is the reading or its double/half.
            let folded = [t.bpm, t.bpm * 2.0, t.bpm / 2.0]
                .into_iter()
                .any(|b| (b - bpm).abs() < 1.0);
            assert!(folded, "{bpm} read as {}", t.bpm);
        }
    }

    #[test]
    fn eighth_notes_do_not_halve_a_fast_tempo() {
        // 170 BPM with softer off-beats: the drum'n'bass case that
        // version 1 read as 85.
        let t = tempo_of(&clicks(170.0, 40.0, 0.4));
        assert!((t.bpm - 170.0).abs() < 2.0, "read {}", t.bpm);
    }

    #[test]
    fn a_steady_click_is_trusted() {
        let t = tempo_of(&clicks(120.0, 40.0, 0.3));
        assert!(t.stability > 0.9, "{t:?}");
        assert!(t.confidence > 0.5, "{t:?}");
    }

    #[test]
    fn noise_is_not_trusted() {
        let mut seed = 7u32;
        let noise: Vec<f32> = (0..(40.0 * RATE) as usize)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed as f32 / u32::MAX as f32) * 0.2 - 0.1
            })
            .collect();
        if let Some(t) = analyse(&noise).tempo {
            assert!(t.confidence < 0.3, "{t:?}");
        }
    }

    #[test]
    fn silence_and_short_clips_have_no_tempo() {
        assert!(analyse(&vec![0.0; (40.0 * RATE) as usize]).tempo.is_none());
        assert!(analyse(&clicks(120.0, 3.0, 0.0)).tempo.is_none());
    }

    #[test]
    fn a_tempo_change_lowers_stability() {
        let mut signal = clicks(90.0, 30.0, 0.0);
        signal.extend(clicks(133.0, 30.0, 0.0));
        let steady = tempo_of(&clicks(90.0, 60.0, 0.0));
        let changing = tempo_of(&signal);
        assert!(
            changing.stability < steady.stability,
            "{changing:?} vs {steady:?}"
        );
    }

    #[test]
    fn energy_ignores_the_level() {
        let loud = clicks(128.0, 30.0, 0.5);
        let quiet: Vec<f32> = loud.iter().map(|s| s * 0.1).collect();
        let a = analyse(&loud).energy.expect("energy");
        let b = analyse(&quiet).energy.expect("energy");
        assert!((a.value - b.value).abs() < 0.03, "{a:?} vs {b:?}");
    }

    #[test]
    fn dense_attacks_are_more_energetic_than_sparse_ones() {
        let dense = analyse(&clicks(170.0, 30.0, 0.7)).energy.expect("energy");
        let sparse = analyse(&clicks(60.0, 30.0, 0.0)).energy.expect("energy");
        assert!(dense.value > sparse.value, "{dense:?} vs {sparse:?}");
    }
}
