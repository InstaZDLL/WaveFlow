//! A gain per frequency band, applied so that full gain changes nothing.
//!
//! [`BandGains`] works on short-time spectra: Hann windows of about 43 ms,
//! a quarter of one apart, each bin weighted by the gains of the bands it
//! belongs to, then overlapped back. The bands are masks over the bins that
//! add up to one everywhere (raised-cosine edges, a sixth of an octave either
//! side of each cut-off), so with every gain at 1 the output is the input,
//! delayed by one window, to float rounding. A band lowered keeps its edges
//! clean: a tone two octaves under a cut-off stays wholly in its band.
//!
//! Two time-domain splits were measured and dropped. A Linkwitz-Riley tree
//! adds up to an all-pass: the energy is kept but not the waveform, and the
//! peaks of a limited master went from 1.075 to 2.348. Bands taken by
//! subtracting low-passes add up exactly, but leak: the difference of two
//! filters of different phase is not a clean band, and a 40 Hz tone put 24 %
//! of its energy in the 150–600 Hz band.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

/// Most bands a split holds, so gains fit in a fixed array.
pub const MAX_BANDS: usize = 8;

/// Half the width of a band edge, in octaves.
const EDGE_OCTAVES: f64 = 1.0 / 6.0;

/// A two-pole filter in transposed direct form II, one state per channel.
#[derive(Debug, Clone)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: Vec<[f32; 2]>,
}

impl Biquad {
    /// A filter that passes everything, for `channels` channels.
    pub fn new(channels: usize) -> Self {
        Self {
            b: [1.0, 0.0, 0.0],
            a: [0.0, 0.0],
            z: vec![[0.0; 2]; channels.max(1)],
        }
    }

    fn set(&mut self, high: bool, freq: f64, q: f64, rate: u32) {
        let rate = f64::from(rate.max(1));
        let w = 2.0 * std::f64::consts::PI * freq.clamp(1.0, 0.45 * rate) / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q.max(0.1));
        let a0 = 1.0 + alpha;
        let (b0, b1) = if high {
            ((1.0 + cos) / 2.0, -(1.0 + cos))
        } else {
            ((1.0 - cos) / 2.0, 1.0 - cos)
        };
        self.b = [(b0 / a0) as f32, (b1 / a0) as f32, (b0 / a0) as f32];
        self.a = [(-2.0 * cos / a0) as f32, ((1.0 - alpha) / a0) as f32];
    }

    /// RBJ cookbook low-pass at `freq`, quality `q`. The state is kept, so a
    /// filter can be swept while it runs.
    pub fn set_low_pass(&mut self, freq: f64, q: f64, rate: u32) {
        self.set(false, freq, q, rate);
    }

    /// RBJ cookbook high-pass at `freq`, quality `q`.
    pub fn set_high_pass(&mut self, freq: f64, q: f64, rate: u32) {
        self.set(true, freq, q, rate);
    }

    /// One sample of channel `ch`.
    #[inline]
    pub fn run(&mut self, ch: usize, x: f32) -> f32 {
        let z = &mut self.z[ch];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The window length at `rate`: about 43 ms, a power of two.
pub fn window_len(rate: u32) -> usize {
    ((rate / 24).max(64) as usize).next_power_of_two()
}

/// How much of each bin belongs to each band: raised-cosine edges in log
/// frequency around each cut-off, adding up to one in every bin.
fn masks(cutoffs: &[f64], bins: usize, n: usize, rate: u32) -> Vec<[f32; MAX_BANDS]> {
    let above = |f: f64, c: f64| -> f64 {
        if f <= 0.0 {
            return 0.0;
        }
        let t = ((f / c).log2() / EDGE_OCTAVES + 1.0) / 2.0;
        let t = t.clamp(0.0, 1.0);
        0.5 - 0.5 * (std::f64::consts::PI * t).cos()
    };
    (0..bins)
        .map(|bin| {
            let f = bin as f64 * f64::from(rate) / n as f64;
            let mut m = [0.0_f32; MAX_BANDS];
            let mut prev = 1.0;
            for (k, c) in cutoffs.iter().enumerate() {
                let a = above(f, *c);
                m[k] = (prev - a) as f32;
                prev = a;
            }
            m[cutoffs.len()] = prev as f32;
            m
        })
        .collect()
}

/// Band gains on any channel count, the same on every channel. Allocates
/// when built, never after.
pub struct BandGains {
    channels: usize,
    n: usize,
    hop: usize,
    bands: usize,
    masks: Vec<[f32; MAX_BANDS]>,
    window: Vec<f32>,
    gains: [f32; MAX_BANDS],
    /// The last `n` input frames and the output being overlapped, both rings
    /// of `n` frames, interleaved, at the same slot for the same frame.
    input: Vec<f32>,
    output: Vec<f32>,
    pos: usize,
    count: u64,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    frame: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch_fwd: Vec<Complex<f32>>,
    scratch_inv: Vec<Complex<f32>>,
}

impl BandGains {
    /// Bands split at `cutoffs` (Hz, at most [`MAX_BANDS`] − 1 kept, sorted),
    /// every gain at 1.
    pub fn new(cutoffs: &[f64], channels: usize, rate: u32) -> Self {
        let mut cuts: Vec<f64> = cutoffs.iter().copied().filter(|f| *f > 0.0).collect();
        cuts.sort_by(f64::total_cmp);
        cuts.truncate(MAX_BANDS - 1);
        let channels = channels.max(1);
        let n = window_len(rate);
        let bins = n / 2 + 1;
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let window = (0..n)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32)
            .collect();
        Self {
            channels,
            n,
            hop: n / 4,
            bands: cuts.len() + 1,
            masks: masks(&cuts, bins, n, rate),
            window,
            gains: [1.0; MAX_BANDS],
            input: vec![0.0; n * channels],
            output: vec![0.0; n * channels],
            pos: 0,
            count: 0,
            frame: fwd.make_input_vec(),
            spectrum: fwd.make_output_vec(),
            scratch_fwd: fwd.make_scratch_vec(),
            scratch_inv: inv.make_scratch_vec(),
            fwd,
            inv,
        }
    }

    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Frames between an input and its output: one window.
    pub fn latency(&self) -> usize {
        self.n
    }

    /// The gains of the next window to be worked out, lowest band first. A
    /// window covers the last [`Self::latency`] frames given: for a gain to
    /// apply at a moment, set it half a window after that moment.
    pub fn set_gains(&mut self, gains: &[f32]) {
        for (g, v) in self.gains.iter_mut().zip(gains) {
            *g = *v;
        }
    }

    /// One frame in, the frame [`Self::latency`] frames older out.
    pub fn frame(&mut self, input: &[f32], output: &mut [f32]) {
        let ch = self.channels;
        let slot = self.pos * ch;
        for c in 0..ch {
            output[c] = self.output[slot + c];
            self.output[slot + c] = 0.0;
            self.input[slot + c] = input[c];
        }
        self.pos = (self.pos + 1) % self.n;
        self.count += 1;
        if self.count % self.hop as u64 == 0 {
            self.window_out();
        }
    }

    /// Weigh the window of the last `n` frames and overlap it into the
    /// output, at the slots those frames came in on.
    fn window_out(&mut self) {
        let (n, ch) = (self.n, self.channels);
        // Hann analysis and synthesis a quarter apart add up to 1.5; the
        // inverse transform leaves a factor `n`.
        let scale = 1.0 / (1.5 * n as f32);
        let gains = self.gains;
        let bands = self.bands;
        for c in 0..ch {
            for i in 0..n {
                self.frame[i] = self.input[((self.pos + i) % n) * ch + c] * self.window[i];
            }
            if self
                .fwd
                .process_with_scratch(&mut self.frame, &mut self.spectrum, &mut self.scratch_fwd)
                .is_err()
            {
                continue;
            }
            for (bin, s) in self.spectrum.iter_mut().enumerate() {
                let m = &self.masks[bin];
                let g: f32 = (0..bands).map(|b| m[b] * gains[b]).sum();
                *s *= g;
            }
            // The transform wants the edge bins real.
            self.spectrum[0].im = 0.0;
            self.spectrum[n / 2].im = 0.0;
            if self
                .inv
                .process_with_scratch(&mut self.spectrum, &mut self.frame, &mut self.scratch_inv)
                .is_err()
            {
                continue;
            }
            for i in 0..n {
                self.output[((self.pos + i) % n) * ch + c] +=
                    self.frame[i] * self.window[i] * scale;
            }
        }
    }
}

/// Mean power of each band of `pcm` (interleaved, `channels`) over frames
/// `from..to`, on the masks [`BandGains`] uses: how much of a track's power
/// each band holds, to keep the mix's power ([`super::level::keep_power`]).
pub fn band_powers(
    pcm: &[f32],
    channels: usize,
    rate: u32,
    cutoffs: &[f64],
    from: usize,
    to: usize,
) -> Vec<f64> {
    let mut cuts: Vec<f64> = cutoffs.iter().copied().filter(|f| *f > 0.0).collect();
    cuts.sort_by(f64::total_cmp);
    cuts.truncate(MAX_BANDS - 1);
    let channels = channels.max(1);
    let n = window_len(rate);
    let masks = masks(&cuts, n / 2 + 1, n, rate);
    let fwd = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut frame = fwd.make_input_vec();
    let mut spectrum = fwd.make_output_vec();
    let frames = pcm.len() / channels;
    let mut power = vec![0.0; cuts.len() + 1];
    let mut windows = 0.0;
    let mut f = from;
    while f + n <= to.min(frames) {
        for c in 0..channels {
            for i in 0..n {
                let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos();
                frame[i] = pcm[(f + i) * channels + c] * w;
            }
            if fwd.process(&mut frame, &mut spectrum).is_err() {
                continue;
            }
            for (bin, s) in spectrum.iter().enumerate() {
                let p = f64::from(s.norm_sqr());
                for (b, pw) in power.iter_mut().enumerate() {
                    *pw += p * f64::from(masks[bin][b]).powi(2);
                }
            }
        }
        windows += 1.0;
        f += n;
    }
    if windows > 0.0 {
        // To mean square per sample and channel: half a spectrum holds half
        // of Parseval's `n` times the windowed energy, and a Hann window
        // keeps 0.375 of it.
        let norm = windows * channels as f64 * n as f64 * n as f64 * 0.1875;
        for p in &mut power {
            *p /= norm;
        }
    }
    power
}
