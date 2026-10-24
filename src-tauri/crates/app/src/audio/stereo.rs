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
mod tests {
    use super::*;

    const SAMPLES: [f32; 8] = [0.5, -0.25, 0.8, 0.1, -0.6, -0.3, 0.123_456_7, 0.987_654_3];

    fn run(balance: i16, width: i16, channels: usize, input: &[f32]) -> Vec<f32> {
        let shared = StereoShared::new();
        shared.set_balance(i32::from(balance));
        shared.set_width(i32::from(width));
        let mut buf = input.to_vec();
        StereoProcessor::new().process(&mut buf, channels, &shared, false, false);
        buf
    }

    fn bits(v: &[f32]) -> Vec<u32> {
        v.iter().map(|s| s.to_bits()).collect()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn neutral_is_the_identity_bit_for_bit() {
        assert_eq!(bits(&run(0, WIDTH_NEUTRAL, 2, &SAMPLES)), bits(&SAMPLES));
    }

    #[test]
    fn hard_right_silences_the_left_and_leaves_the_right_alone() {
        let out = run(BALANCE_LIMIT, WIDTH_NEUTRAL, 2, &[0.5, -0.25, 0.8, 0.1]);
        // The far gain is cos(π/2), a hair off zero in f32.
        assert!(out[0].abs() < 1e-7 && out[2].abs() < 1e-7, "{out:?}");
        assert!(close(out[1], -0.25) && close(out[3], 0.1), "{out:?}");
    }

    #[test]
    fn hard_left_silences_the_right() {
        let out = run(-BALANCE_LIMIT, WIDTH_NEUTRAL, 2, &[0.5, -0.25]);
        assert!(close(out[0], 0.5), "{out:?}");
        assert!(out[1].abs() < 1e-7, "{out:?}");
    }

    #[test]
    fn half_travel_takes_three_db_off_the_far_side() {
        let out = run(50, WIDTH_NEUTRAL, 2, &[1.0, 1.0]);
        assert!(close(out[0], std::f32::consts::FRAC_1_SQRT_2), "{out:?}");
        assert!(close(out[1], 1.0), "{out:?}");
    }

    #[test]
    fn balance_never_raises_either_channel() {
        for b in -BALANCE_LIMIT..=BALANCE_LIMIT {
            let g = StereoGains::from_settings(b, WIDTH_NEUTRAL);
            assert!(g.left <= 1.0 && g.right <= 1.0, "balance {b}: {g:?}");
        }
    }

    #[test]
    fn zero_width_is_mono() {
        let out = run(0, 0, 2, &[0.5, -0.25, 0.8, 0.1]);
        assert!(close(out[0], 0.125) && close(out[1], 0.125), "{out:?}");
        assert!(close(out[2], 0.45) && close(out[3], 0.45), "{out:?}");
    }

    #[test]
    fn full_width_doubles_the_side_and_halves_the_whole() {
        let out = run(0, WIDTH_MAX, 2, &[0.5, -0.25]);
        // M = 0.125, S = 0.375 → doubled 0.75: (0.875, -0.625), then the
        // 1 / w headroom.
        assert!(close(out[0], 0.4375) && close(out[1], -0.3125), "{out:?}");
    }

    #[test]
    fn widening_never_takes_a_full_scale_frame_past_unity() {
        let frames = [1.0, -1.0, 0.95, 0.2, -1.0, 0.0, 0.99, 0.5, 1.0, 1.0];
        for width in [101, 120, 150, 199, WIDTH_MAX] {
            let out = run(0, width, 2, &frames);
            assert!(out.iter().all(|s| s.abs() <= 1.0), "{width}: {out:?}");
        }
        // The reported case: 150 % on L 0.95, R 0.2 came to 1.1375.
        let out = run(0, 150, 2, &[0.95, 0.2]);
        assert!(close(out[0], 1.1375 / 1.5), "{out:?}");
    }

    #[test]
    fn a_ramp_into_a_wide_setting_stays_under_unity_on_every_frame() {
        let mut p = StereoProcessor::new();
        let mut first = vec![1.0_f32, -1.0];
        p.process_towards(&mut first, 2, StereoGains::NEUTRAL);
        let mut ramp: Vec<f32> = [1.0_f32, -1.0].repeat(64);
        p.process_towards(&mut ramp, 2, StereoGains::from_settings(0, WIDTH_MAX));
        assert!(ramp.iter().all(|s| s.abs() <= 1.0), "{ramp:?}");
    }

    #[test]
    fn a_mono_signal_comes_out_exactly_as_it_went_in_up_to_full_width() {
        let mono: Vec<f32> = SAMPLES.iter().flat_map(|&s| [s, s]).collect();
        for width in [0, 37, WIDTH_NEUTRAL] {
            assert_eq!(bits(&run(0, width, 2, &mono)), bits(&mono), "{width}");
        }
    }

    #[test]
    fn mono_folds_before_the_balance_moves_it() {
        let shared = StereoShared::new();
        shared.set_balance(i32::from(BALANCE_LIMIT));
        // Hard-panned left: with mono on and the balance hard right, the
        // source's left channel has to reach the right speaker.
        let mut buf = vec![1.0_f32, 0.0];
        StereoProcessor::new().process(&mut buf, 2, &shared, true, false);
        assert!(buf[0].abs() < 1e-7, "{buf:?}");
        assert!(close(buf[1], 0.5), "{buf:?}");
    }

    #[test]
    fn a_mono_source_or_the_mono_switch_is_not_widened() {
        let shared = StereoShared::new();
        shared.set_width(i32::from(WIDTH_MAX));
        let mono: Vec<f32> = SAMPLES.iter().flat_map(|&s| [s, s]).collect();
        let mut buf = mono.clone();
        StereoProcessor::new().process(&mut buf, 2, &shared, false, true);
        assert_eq!(bits(&buf), bits(&mono), "a mono track keeps its level");
        let mut folded = vec![0.5_f32, 0.5];
        StereoProcessor::new().process(&mut folded, 2, &shared, true, false);
        assert_eq!(folded, vec![0.5, 0.5]);
    }

    #[test]
    fn mono_averages_every_channel_of_the_frame() {
        let mut buf = vec![1.0_f32, 0.0, 0.5, 0.5];
        mono_fold(&mut buf, 4);
        assert_eq!(buf, vec![0.5; 4]);
        let mut single = vec![0.3_f32, -0.2];
        mono_fold(&mut single, 1);
        assert_eq!(single, vec![0.3, -0.2]);
    }

    #[test]
    fn mono_off_and_neutral_settings_leave_the_buffer_alone() {
        let shared = StereoShared::new();
        let mut buf = SAMPLES.to_vec();
        StereoProcessor::new().process(&mut buf, 2, &shared, false, false);
        assert_eq!(bits(&buf), bits(&SAMPLES));
    }

    #[test]
    fn a_buffer_with_no_whole_frame_keeps_the_change_for_the_next_one() {
        let mut p = StereoProcessor::new();
        let mut first = vec![1.0_f32; 8];
        p.process_towards(&mut first, 2, StereoGains::NEUTRAL);
        let hard_right = StereoGains::from_settings(BALANCE_LIMIT, WIDTH_NEUTRAL);
        p.process_towards(&mut [], 2, hard_right);
        let mut next = vec![1.0_f32; 8];
        p.process_towards(&mut next, 2, hard_right);
        // Still ramps: the first left sample is three quarters, not zero.
        assert!(close(next[0], 0.75), "{next:?}");
    }

    #[test]
    fn a_single_channel_output_is_never_touched() {
        assert_eq!(bits(&run(BALANCE_LIMIT, 0, 1, &SAMPLES)), bits(&SAMPLES));
    }

    #[test]
    fn only_the_front_pair_of_a_wider_output_moves() {
        let frame = [0.5, -0.25, 0.3, 0.2, -0.1, 0.4];
        let out = run(0, 0, 6, &frame);
        assert!(close(out[0], 0.125) && close(out[1], 0.125), "{out:?}");
        assert_eq!(bits(&out[2..]), bits(&frame[2..]));
    }

    #[test]
    fn settings_are_clamped_on_the_way_in() {
        let shared = StereoShared::new();
        shared.set_balance(1_000);
        shared.set_width(-5);
        assert_eq!(shared.balance(), BALANCE_LIMIT);
        assert_eq!(shared.width(), 0);
        shared.set_balance(-1_000);
        shared.set_width(1_000);
        assert_eq!(shared.balance(), -BALANCE_LIMIT);
        assert_eq!(shared.width(), WIDTH_MAX);
    }

    #[test]
    fn a_change_ramps_across_one_buffer_then_holds() {
        let mut p = StereoProcessor::new();
        let neutral = StereoGains::NEUTRAL;
        let hard_right = StereoGains::from_settings(BALANCE_LIMIT, WIDTH_NEUTRAL);
        let mut first = vec![1.0_f32; 8];
        p.process_towards(&mut first, 2, neutral);
        assert_eq!(first, vec![1.0; 8]);

        let mut ramp = vec![1.0_f32; 8];
        p.process_towards(&mut ramp, 2, hard_right);
        let lefts: Vec<f32> = ramp.iter().step_by(2).copied().collect();
        // Falls one quarter per frame, reaching the target on the last.
        assert!(close(lefts[0], 0.75) && close(lefts[1], 0.5), "{lefts:?}");
        assert!(lefts[3].abs() < 1e-7, "{lefts:?}");
        assert!(ramp.iter().skip(1).step_by(2).all(|&r| close(r, 1.0)));

        let mut held = vec![1.0_f32; 4];
        p.process_towards(&mut held, 2, hard_right);
        assert!(held[0].abs() < 1e-7 && close(held[1], 1.0), "{held:?}");
    }

    #[test]
    fn the_first_buffer_starts_at_its_target() {
        let mut p = StereoProcessor::new();
        let mut buf = vec![1.0_f32; 8];
        p.process_towards(
            &mut buf,
            2,
            StereoGains::from_settings(BALANCE_LIMIT, WIDTH_NEUTRAL),
        );
        assert!(buf.iter().step_by(2).all(|l| l.abs() < 1e-7), "{buf:?}");
    }

    #[test]
    fn coming_back_to_neutral_ends_untouched() {
        let mut p = StereoProcessor::new();
        let mut buf = SAMPLES.to_vec();
        p.process_towards(&mut buf, 2, StereoGains::from_settings(0, 0));
        let mut back = SAMPLES.to_vec();
        p.process_towards(&mut back, 2, StereoGains::NEUTRAL);
        // The ramp buffer moved; the one after it is the identity.
        let mut after = SAMPLES.to_vec();
        p.process_towards(&mut after, 2, StereoGains::NEUTRAL);
        assert_eq!(bits(&after), bits(&SAMPLES));
    }
}
