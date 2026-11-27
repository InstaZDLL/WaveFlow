//! Keeping the level where it was while both tracks sound.
//!
//! Two uncorrelated tracks at full gain add their powers: a transition that
//! never turns the overall volume down (bands handed over one at a time,
//! both sides full in some of them) comes out louder in its middle and
//! peaks over full scale. The first answer is headroom — [`keep_power`]
//! turns the sum down by what the overlap adds — and the [`Limiter`] only
//! catches what is still over, and counts it.

use std::collections::VecDeque;

/// The gain that takes back the power the overlap adds, and only that.
///
/// `out_gains`, `in_gains` are the current gains per band, `out_power`,
/// `in_power` each track's mean power in those bands over the transition, at
/// the level the mix plays them (one band for a transition without bands).
/// In each band the two gains are brought back to a power of one at most
/// — both full counts as each at -3 dB, one side alone is left as it is — so
/// a band that one track holds on its own, as a bass kept to the last beat,
/// is not turned down for being loud. Only ever turns down, by no more than
/// `floor` (a linear gain), and by the share `strength` of what it would
/// (0 is none, 1 all of it).
///
/// A first version aimed at the power an equal-power crossfade of the two
/// whole tracks would have at that point: on a pair whose outgoing track
/// held seven times the incoming one's bass, it fought the template that
/// keeps that bass to the end, and turned the whole mix down by its floor.
pub fn keep_power(
    out_gains: &[f32],
    in_gains: &[f32],
    out_power: &[f64],
    in_power: &[f64],
    strength: f64,
    floor: f64,
) -> f32 {
    let mut expected = 0.0;
    let mut target = 0.0;
    for (b, (po, pi)) in out_power.iter().zip(in_power).enumerate() {
        let go = f64::from(out_gains.get(b).copied().unwrap_or(0.0)).powi(2);
        let gi = f64::from(in_gains.get(b).copied().unwrap_or(0.0)).powi(2);
        let band = go * po + gi * pi;
        expected += band;
        target += band / (go + gi).max(1.0);
    }
    if expected <= 1e-12 {
        return 1.0;
    }
    let full = (target / expected).sqrt().clamp(floor.clamp(0.0, 1.0), 1.0);
    full.powf(strength.clamp(0.0, 1.0)) as f32
}

/// A look-ahead peak limiter with one gain for all channels, so the image
/// stays put: the gain a peak needs is held for the look-ahead before it,
/// and eased back up after. A last resort, and it says how often it acted:
/// [`Self::touched`] counts the frames it turned down.
#[derive(Debug, Clone)]
pub struct Limiter {
    channels: usize,
    ceiling: f32,
    length: usize,
    /// The frames waiting, interleaved, a ring of `length`.
    delay: Vec<f32>,
    held: usize,
    cursor: usize,
    /// Falling needed gains of the last `length` frames: the front is their
    /// minimum.
    minima: VecDeque<(u64, f32)>,
    frame: u64,
    eased: f32,
    release: f32,
    touched: u64,
}

impl Limiter {
    /// Under `ceiling`, looking `lookahead_s` ahead, coming back over
    /// `release_s`.
    pub fn new(channels: usize, rate: u32, ceiling: f32, lookahead_s: f64, release_s: f64) -> Self {
        let channels = channels.max(1);
        let length = ((lookahead_s * f64::from(rate)) as usize).max(1);
        let release = 1.0 - (-1.0 / (release_s.max(1e-3) * f64::from(rate))).exp() as f32;
        Self {
            channels,
            ceiling,
            length,
            delay: vec![0.0; length * channels],
            held: 0,
            cursor: 0,
            minima: VecDeque::with_capacity(length + 1),
            frame: 0,
            eased: 1.0,
            release,
            touched: 0,
        }
    }

    /// Frames of delay the look-ahead adds.
    pub fn latency(&self) -> usize {
        self.length
    }

    /// Frames turned down so far.
    pub fn touched(&self) -> u64 {
        self.touched
    }

    /// Take one frame in, give the one `latency` frames older back in
    /// `io`; silence until the look-ahead has filled.
    pub fn frame(&mut self, io: &mut [f32]) {
        let peak = io.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
        let needed = if peak > self.ceiling {
            self.ceiling / peak
        } else {
            1.0
        };
        while self.minima.back().is_some_and(|(_, g)| *g >= needed) {
            self.minima.pop_back();
        }
        self.minima.push_back((self.frame, needed));
        let oldest = (self.frame + 1).saturating_sub(self.length as u64 + 1);
        while self.minima.front().is_some_and(|(at, _)| *at < oldest) {
            self.minima.pop_front();
        }
        let hold = self.minima.front().map_or(1.0, |(_, g)| *g);
        self.eased = hold.min(self.eased + (1.0 - self.eased) * self.release);
        self.frame += 1;

        let slot = self.cursor * self.channels;
        if self.held < self.length {
            self.delay[slot..slot + self.channels].copy_from_slice(io);
            io.fill(0.0);
            self.held += 1;
        } else {
            for c in 0..self.channels {
                let out = self.delay[slot + c] * self.eased;
                self.delay[slot + c] = io[c];
                io[c] = out;
            }
            if self.eased < 0.9999 {
                self.touched += 1;
            }
        }
        self.cursor = (self.cursor + 1) % self.length;
    }
}
