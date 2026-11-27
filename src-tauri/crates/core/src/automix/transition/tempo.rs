//! A track's tempo moving over a transition, on either side.
//!
//! A [`Glide`] is a speed — seconds of the track per second of output — as
//! a function of the track's own position: `from` until `start`, `to` from
//! `end`, a smoothstep between. Defining it over the track's time rather
//! than the output's makes a beat of the track a fixed thing to place: the
//! output time it lands at is [`Glide::output_time`] from the start.

/// Step of the numeric integration over the moving part, in track seconds.
/// The speed bends by a few percent over beats; at a millisecond the error
/// is far under a sample.
const STEP_S: f64 = 0.001;

/// Which track a transition stretches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Outgoing,
    Incoming,
}

/// A speed over a track's own time: `from` before `start`, `to` after
/// `end`, a smoothstep between.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glide {
    pub from: f64,
    pub to: f64,
    pub start: f64,
    pub end: f64,
}

impl Glide {
    /// One speed throughout.
    pub fn constant(speed: f64) -> Self {
        Self {
            from: speed,
            to: speed,
            start: 0.0,
            end: 0.0,
        }
    }

    /// The speed at track position `s`.
    pub fn speed(&self, s: f64) -> f64 {
        if self.end <= self.start {
            return if s < self.start { self.from } else { self.to };
        }
        if s <= self.start {
            return self.from;
        }
        if s >= self.end {
            return self.to;
        }
        let u = (s - self.start) / (self.end - self.start);
        self.from + (self.to - self.from) * u * u * (3.0 - 2.0 * u)
    }

    /// Output seconds the track takes from position `a` to position `b`.
    pub fn output_time(&self, a: f64, b: f64) -> f64 {
        if b < a {
            return -self.output_time(b, a);
        }
        let mut t = 0.0;
        // The steady parts, exactly.
        let before_end = b.min(self.start);
        if before_end > a {
            t += (before_end - a) / self.from;
        }
        let after_start = a.max(self.end.max(self.start));
        if b > after_start {
            t += (b - after_start) / self.to;
        }
        // The moving part, by midpoints.
        let (ra, rb) = (a.max(self.start), b.min(self.end));
        if rb > ra {
            let n = ((rb - ra) / STEP_S).ceil().max(1.0) as usize;
            let h = (rb - ra) / n as f64;
            t += (0..n)
                .map(|k| h / self.speed(ra + (k as f64 + 0.5) * h))
                .sum::<f64>();
        }
        t
    }

    /// The track position reached `t` output seconds after position `a`.
    pub fn track_time(&self, a: f64, t: f64) -> f64 {
        // The output time grows with the position: bisect.
        let fastest = self.from.max(self.to);
        let (mut lo, mut hi) = (a, a + t.max(0.0) * fastest + 1e-9);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if self.output_time(a, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }
}
