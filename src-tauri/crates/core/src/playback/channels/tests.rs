#[test]
fn mono_on_a_wide_output_plays_from_the_front_pair() {
    let mut out = Vec::new();
    super::convert_channels(&[0.5, -0.25], 1, 8, &mut out);
    assert_eq!(
        out,
        [
            0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, //
            -0.25, -0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ]
    );
}

use super::{convert_channels, downmix_frame_to_stereo};

const K: f32 = 0.707;

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
}

#[test]
fn equal_channels_pass_through() {
    let mut out = Vec::new();
    convert_channels(&[0.1, -0.2, 0.3, -0.4], 2, 2, &mut out);
    assert_eq!(out, vec![0.1, -0.2, 0.3, -0.4]);
}

#[test]
fn mono_to_stereo_duplicates() {
    let mut out = Vec::new();
    convert_channels(&[0.5, -0.5], 1, 2, &mut out);
    assert_eq!(out, vec![0.5, 0.5, -0.5, -0.5]);
}

#[test]
fn stereo_to_mono_averages() {
    let mut out = Vec::new();
    convert_channels(&[1.0, 0.0], 2, 1, &mut out);
    approx(out[0], 0.5);
}

#[test]
fn five_one_uses_bs775_and_drops_lfe() {
    // FL FR FC LFE BL BR
    let (lo, ro) = downmix_frame_to_stereo(&[1.0, 2.0, 0.4, 9.9, 0.6, 0.8]);
    approx(lo, 1.0 + K * 0.4 + K * 0.6);
    approx(ro, 2.0 + K * 0.4 + K * 0.8);
}

#[test]
fn five_zero_keeps_centre_and_surrounds() {
    // The regression this fix targets: 5.0 (FL FR FC BL BR) must
    // NOT be truncated to the front pair.
    let mut out = Vec::new();
    convert_channels(&[1.0, 2.0, 0.4, 0.6, 0.8], 5, 2, &mut out);
    approx(out[0], 1.0 + K * 0.4 + K * 0.6);
    approx(out[1], 2.0 + K * 0.4 + K * 0.8);
}

#[test]
fn three_zero_keeps_centre() {
    let (lo, ro) = downmix_frame_to_stereo(&[1.0, 2.0, 0.4]);
    approx(lo, 1.0 + K * 0.4);
    approx(ro, 2.0 + K * 0.4);
}

#[test]
fn quad_maps_surrounds_by_side() {
    // FL FR BL BR — no centre.
    let (lo, ro) = downmix_frame_to_stereo(&[1.0, 2.0, 0.6, 0.8]);
    approx(lo, 1.0 + K * 0.6);
    approx(ro, 2.0 + K * 0.8);
}

#[test]
fn six_one_folds_rear_centre_into_both() {
    // FL FR FC LFE BL BR BC — LFE (9.9) dropped, BC summed into L+R.
    let (lo, ro) = downmix_frame_to_stereo(&[1.0, 2.0, 0.4, 9.9, 0.6, 0.8, 0.3]);
    approx(lo, 1.0 + K * 0.4 + K * 0.6 + K * 0.3);
    approx(ro, 2.0 + K * 0.4 + K * 0.8 + K * 0.3);
}

#[test]
fn seven_one_folds_all_surrounds() {
    // FL FR FC LFE BL BR SL SR
    let (lo, ro) = downmix_frame_to_stereo(&[1.0, 2.0, 0.4, 9.9, 0.6, 0.8, 0.1, 0.2]);
    approx(lo, 1.0 + K * 0.4 + K * 0.6 + K * 0.1);
    approx(ro, 2.0 + K * 0.4 + K * 0.8 + K * 0.2);
}

#[test]
fn beyond_seven_one_drops_no_channel() {
    // 10 channels: idx ≥ 8 must still fold in (parity left/right),
    // not be silently dropped.
    let (lo, ro) = downmix_frame_to_stereo(&[1.0, 2.0, 0.4, 9.9, 0.6, 0.8, 0.1, 0.2, 0.05, 0.07]);
    approx(lo, 1.0 + K * (0.4 + 0.6 + 0.1 + 0.05));
    approx(ro, 2.0 + K * (0.4 + 0.8 + 0.2 + 0.07));
}
