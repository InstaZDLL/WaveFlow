//! What can be done to a track on its way out: an echo, a filter sweep, a
//! loop of its last beats.

use super::bands::Biquad;
use super::envelope::Ramp;

/// A feedback delay, every repeat through a band-pass so the echoes stay
/// clear of the incoming track's bass and top. The delay is a whole number
/// of frames, set once: on the beat grid, a fraction of a beat.
#[derive(Debug, Clone)]
pub struct Echo {
    channels: usize,
    delay: usize,
    buf: Vec<f32>,
    at: usize,
    feedback: f32,
    low_cut: Biquad,
    high_cut: Biquad,
}

impl Echo {
    /// `delay` frames, each repeat `feedback` times the one before, through
    /// `low_cut..high_cut` Hz.
    pub fn new(
        delay: usize,
        channels: usize,
        rate: u32,
        feedback: f32,
        low_cut: f64,
        high_cut: f64,
    ) -> Self {
        let channels = channels.max(1);
        let delay = delay.max(1);
        let mut lc = Biquad::new(channels);
        lc.set_high_pass(low_cut, std::f64::consts::FRAC_1_SQRT_2, rate);
        let mut hc = Biquad::new(channels);
        hc.set_low_pass(high_cut, std::f64::consts::FRAC_1_SQRT_2, rate);
        Self {
            channels,
            delay,
            buf: vec![0.0; delay * channels],
            at: 0,
            feedback: feedback.clamp(0.0, 0.95),
            low_cut: lc,
            high_cut: hc,
        }
    }

    /// One frame: `send` goes into the delay, `wet` gets what comes out of it.
    #[inline]
    pub fn frame(&mut self, send: &[f32], wet: &mut [f32]) {
        let base = self.at * self.channels;
        for c in 0..self.channels {
            let read = self.buf[base + c];
            let w = self.high_cut.run(c, self.low_cut.run(c, read));
            wet[c] = w;
            self.buf[base + c] = send[c] + w * self.feedback;
        }
        self.at = (self.at + 1) % self.delay;
    }
}

/// A filter whose cut-off moves from `from_hz` to `to_hz`, evenly on a
/// log scale, along `ramp` over the transition: a high-pass rising takes
/// the outgoing track's body away from the bottom up, a low-pass opening
/// brings the incoming one in from the bottom up.
#[derive(Debug, Clone)]
pub struct Sweep {
    filter: Biquad,
    high_pass: bool,
    from_hz: f64,
    to_hz: f64,
    ramp: Ramp,
    q: f64,
    rate: u32,
    last_u: f64,
}

impl Sweep {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        high_pass: bool,
        from_hz: f64,
        to_hz: f64,
        ramp: Ramp,
        q: f64,
        channels: usize,
        rate: u32,
    ) -> Self {
        let mut this = Self {
            filter: Biquad::new(channels),
            high_pass,
            from_hz: from_hz.max(1.0),
            to_hz: to_hz.max(1.0),
            ramp,
            q,
            rate,
            last_u: f64::NAN,
        };
        this.at(0.0);
        this
    }

    /// Move the cut-off to where it is at `u`. Cheap to call every frame:
    /// the coefficients are worked out again only when `u` has moved by a
    /// thousandth.
    pub fn at(&mut self, u: f64) {
        if (u - self.last_u).abs() < 1e-3 {
            return;
        }
        self.last_u = u;
        let p = self.ramp.rise(u);
        let freq = self.from_hz * (self.to_hz / self.from_hz).powf(p);
        if self.high_pass {
            self.filter.set_high_pass(freq, self.q, self.rate);
        } else {
            self.filter.set_low_pass(freq, self.q, self.rate);
        }
    }

    #[inline]
    pub fn run(&mut self, ch: usize, x: f32) -> f32 {
        self.filter.run(ch, x)
    }
}

/// How long each loop repeat is faded in and out, in seconds, so its cut is
/// not a click.
pub const LOOP_EDGE_S: f64 = 0.004;

/// The source ranges, in seconds of the outgoing track, that a loop plays
/// after `end`: one per entry of `lengths`, that many beats of `period`
/// long, each starting `back` beats before `end`. Halving lengths make a
/// stutter that speeds up. The grid runs on through them unbroken as long
/// as the lengths are whole beats or add up to whole beats.
pub fn loop_segments(end: f64, period: f64, back: f64, lengths: &[f64]) -> Vec<(f64, f64)> {
    let start = end - back * period;
    lengths
        .iter()
        .filter(|l| **l > 0.0)
        .map(|l| (start, start + l * period))
        .collect()
}
