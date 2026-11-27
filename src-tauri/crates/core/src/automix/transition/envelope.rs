//! Gain envelopes over a transition.
//!
//! A transition runs from `u = 0` (its first frame) to `u = 1` (its last).
//! A [`Ramp`] says where between the two a gain moves, and how.

use std::f64::consts::FRAC_PI_2;

/// The shape of a gain's move between the two ends of a [`Ramp`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// Quarter sines: the gain coming up is `sin`, the one going `cos`, so
    /// two uncorrelated signals crossing on the same ramp keep their power.
    EqualPower,
    /// A smoothstep, flat at both ends.
    Smooth,
    /// Straight in amplitude.
    Linear,
}

/// A gain's move over `from..to`, in fractions of the transition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ramp {
    pub from: f64,
    pub to: f64,
    pub shape: Shape,
}

impl Ramp {
    pub const fn new(from: f64, to: f64, shape: Shape) -> Self {
        Self { from, to, shape }
    }

    /// The whole transition, equal power: the plain crossfade.
    pub const fn whole() -> Self {
        Self::new(0.0, 1.0, Shape::EqualPower)
    }

    /// Progress through the ramp at `u`: 0 before it, 1 after it.
    fn progress(&self, u: f64) -> f64 {
        if self.to <= self.from {
            return if u >= self.to { 1.0 } else { 0.0 };
        }
        ((u - self.from) / (self.to - self.from)).clamp(0.0, 1.0)
    }

    /// The gain of a side coming up: 0 before the ramp, 1 after.
    pub fn rise(&self, u: f64) -> f64 {
        let p = self.progress(u);
        match self.shape {
            Shape::EqualPower => (p * FRAC_PI_2).sin(),
            Shape::Smooth => p * p * (3.0 - 2.0 * p),
            Shape::Linear => p,
        }
    }

    /// The gain of a side going: 1 before the ramp, 0 after.
    pub fn fall(&self, u: f64) -> f64 {
        let p = self.progress(u);
        match self.shape {
            Shape::EqualPower => (p * FRAC_PI_2).cos(),
            Shape::Smooth => 1.0 - p * p * (3.0 - 2.0 * p),
            Shape::Linear => 1.0 - p,
        }
    }
}
