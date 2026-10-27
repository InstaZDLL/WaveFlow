//! Mono, balance and stereo width.
//!
//! The stage that decides what each speaker of the front pair gets, on
//! the decoder thread after the EQ and before the final clamp. Like the
//! EQ, it stays out of the cpal callback so its no-alloc / no-lock
//! contract holds; the callback only scales what the ring holds.
//!
//! **Mono** comes first: every channel of a frame gets the frame's
//! average. It used to be done in the output, after everything else, and
//! with a balance that cannot work — the output averaged a hard-right
//! frame back over both speakers at half level, erasing the source's
//! left channel instead of moving the sound. Folded here, the balance
//! then moves the folded signal, which is what mono plus balance is for.
//!
//! **Width** is done in mid/side: `M = (L + R) / 2`, `S = (L − R) / 2`,
//! the side scaled by the width, then `L = M + S`, `R = M − S`. 0 % is
//! mono, 100 % is the source as it is, 200 % doubles the side. A frame
//! with `L == R` has no side, so its output is the input bit for bit at
//! every width up to 100 %: a mono track played on a stereo output stays
//! exactly as it was.
//!
//! **Headroom.** `|M| + |S|` is the larger of `|L|` and `|R|`, so a width
//! `w` above 100 % can raise a peak by up to `w` (+6 dB at 200 %), and a
//! hard-panned part of a loud master would be heard as clipping at the
//! final clamp. Above 100 % the frame is therefore scaled by `1 / w`,
//! computed from the width the frame is played at (so a ramp keeps the
//! bound too). Widening plays quieter, as an EQ boost does; it never
//! clips. At 100 % and below there is nothing to give back.
//!
//! **Balance** attenuates the far channel and leaves the near one at
//! unity: the far gain follows `cos(|b| · π/2)`, so −3 dB at half travel
//! and silence at the end stop. This is not a constant-power pan law on
//! purpose. A constant-power pan normalised to unity at the centre lifts
//! the near side by up to 3 dB, which pushes a full-scale master into the
//! clamp; a balance control trims, it never boosts, so it cannot clip
//! what it is given.
//!
//! **Neutral is no stage at all.** With mono off, balance 0 and width
//! 100 % the buffer is not touched (the mid/side round trip is not
//! bit-exact in floating point), so bit-perfect output is kept. A
//! single-channel output is never touched either: it has no pair to fold,
//! balance or widen. Balance and width leave the further channels of a
//! surround output as they are; mono folds them in, as it always did.
//!
//! **Changes ramp.** A new balance or width moves to its target linearly
//! across one buffer, so a slider dragged during playback does not click;
//! the first buffer a processor sees starts at the target, so a track that
//! opens with the balance hard to one side does not leak the first buffer
//! through the other. The mono switch takes effect on the next buffer, as
//! it did in the output.

use std::f32::consts::FRAC_PI_2;
use std::sync::atomic::{AtomicI16, Ordering};

/// The balance end stop, in percent either side of the centre.
/// Negative is left, positive is right.
pub const BALANCE_LIMIT: i16 = 100;

/// Width in percent: 0 is mono, 100 the source as it is.
pub const WIDTH_NEUTRAL: i16 = 100;

/// The widest setting offered: the side doubled.
pub const WIDTH_MAX: i16 = 200;

/// Atomic shared state — written by the command layer and by the
/// decoder's `Set*` handling, read by the decoder thread once per buffer.
/// Whole percents: the sliders step in percents, and an integer store
/// avoids the f32-as-bits dance.
pub struct StereoShared {
    balance_pct: AtomicI16,
    width_pct: AtomicI16,
}

impl StereoShared {
    pub fn new() -> Self {
        Self {
            balance_pct: AtomicI16::new(0),
            width_pct: AtomicI16::new(WIDTH_NEUTRAL),
        }
    }

    /// Store a balance, clamped to `±BALANCE_LIMIT`.
    pub fn set_balance(&self, pct: i32) {
        let clamped = pct.clamp(-i32::from(BALANCE_LIMIT), i32::from(BALANCE_LIMIT));
        self.balance_pct.store(clamped as i16, Ordering::Release);
    }

    /// Store a width, clamped to `0..=WIDTH_MAX`.
    pub fn set_width(&self, pct: i32) {
        let clamped = pct.clamp(0, i32::from(WIDTH_MAX));
        self.width_pct.store(clamped as i16, Ordering::Release);
    }

    pub fn balance(&self) -> i16 {
        self.balance_pct.load(Ordering::Acquire)
    }

    pub fn width(&self) -> i16 {
        self.width_pct.load(Ordering::Acquire)
    }
}

impl Default for StereoShared {
    fn default() -> Self {
        Self::new()
    }
}

/// The three factors one setting works out to: the side gain and the
/// two output gains.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StereoGains {
    side: f32,
    left: f32,
    right: f32,
}

impl StereoGains {
    const NEUTRAL: Self = Self {
        side: 1.0,
        left: 1.0,
        right: 1.0,
    };

    fn from_settings(balance_pct: i16, width_pct: i16) -> Self {
        let b =
            f32::from(balance_pct.clamp(-BALANCE_LIMIT, BALANCE_LIMIT)) / f32::from(BALANCE_LIMIT);
        // `cos(0)` is exactly 1.0, so the centre leaves both at unity.
        let far = (b.abs() * FRAC_PI_2).cos();
        let (left, right) = if b > 0.0 {
            (far, 1.0)
        } else if b < 0.0 {
            (1.0, far)
        } else {
            (1.0, 1.0)
        };
        Self {
            side: f32::from(width_pct.clamp(0, WIDTH_MAX)) / f32::from(WIDTH_NEUTRAL),
            left,
            right,
        }
    }

    fn lerp(self, to: Self, t: f32) -> Self {
        Self {
            side: self.side + (to.side - self.side) * t,
            left: self.left + (to.left - self.left) * t,
            right: self.right + (to.right - self.right) * t,
        }
    }
}

#[inline(always)]
fn apply_frame(frame: &mut [f32], g: StereoGains) {
    let (l, r) = (frame[0], frame[1]);
    let mid = (l + r) * 0.5;
    let side = (l - r) * 0.5 * g.side;
    // See the module's note on headroom: `1 / w` bounds the widened peak.
    let headroom = if g.side > 1.0 { 1.0 / g.side } else { 1.0 };
    frame[0] = (mid + side) * g.left * headroom;
    frame[1] = (mid - side) * g.right * headroom;
}

/// Replace every channel of each frame with the frame's average. Shared
/// with the output test tone, which is folded the same way when the mono
/// switch is on.
pub fn mono_fold(buf: &mut [f32], channels: usize) {
    if channels < 2 {
        return;
    }
    let scale = 1.0 / channels as f32;
    for frame in buf.chunks_exact_mut(channels) {
        let average = frame.iter().sum::<f32>() * scale;
        frame.fill(average);
    }
}

/// Per-decoder-thread state: the gains the last buffer ended on, so a
/// change can ramp from them.
pub struct StereoProcessor {
    /// `None` until the first buffer, which starts at its target.
    current: Option<StereoGains>,
}

impl StereoProcessor {
    pub fn new() -> Self {
        Self { current: None }
    }

    /// Apply mono, balance and width in place to an interleaved buffer.
    /// Two atomic loads per call; nothing at all is written while mono is
    /// off and the balance and width are, and were, neutral.
    ///
    /// `mono_source` is true when what is decoding has a single channel:
    /// there is no side to widen then, and the width's headroom would only
    /// make the track quieter, so the width is left at 100 % — as it is
    /// when the mono switch has just folded the side away.
    pub fn process(
        &mut self,
        buf: &mut [f32],
        channels: usize,
        shared: &StereoShared,
        mono: bool,
        mono_source: bool,
    ) {
        if mono {
            mono_fold(buf, channels);
        }
        let width = if mono || mono_source {
            WIDTH_NEUTRAL
        } else {
            shared.width()
        };
        let target = StereoGains::from_settings(shared.balance(), width);
        self.process_towards(buf, channels, target);
    }

    fn process_towards(&mut self, buf: &mut [f32], channels: usize, target: StereoGains) {
        // Before anything moves `current`: a buffer with no whole frame
        // (a resampler holding back a short packet) must not use up a
        // change, or the next one would step to it without a ramp.
        if channels < 2 || buf.len() < channels {
            return;
        }
        let start = *self.current.get_or_insert(target);
        self.current = Some(target);
        if start == StereoGains::NEUTRAL && target == StereoGains::NEUTRAL {
            return;
        }
        if start == target {
            for frame in buf.chunks_exact_mut(channels) {
                apply_frame(frame, target);
            }
            return;
        }
        let frames = (buf.len() / channels) as f32;
        for (f, frame) in buf.chunks_exact_mut(channels).enumerate() {
            apply_frame(frame, start.lerp(target, (f + 1) as f32 / frames));
        }
    }
}

impl Default for StereoProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
