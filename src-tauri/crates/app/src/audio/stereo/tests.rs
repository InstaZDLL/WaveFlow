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
