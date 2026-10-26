use super::*;

fn approx_eq(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn equal_power_endpoints_are_pure() {
    let (out0, in0) = equal_power_gains(0.0);
    assert!(approx_eq(out0, 1.0, 1e-6));
    assert!(approx_eq(in0, 0.0, 1e-6));

    let (out1, in1) = equal_power_gains(1.0);
    assert!(approx_eq(out1, 0.0, 1e-6));
    assert!(approx_eq(in1, 1.0, 1e-6));
}

#[test]
fn equal_power_preserves_energy() {
    // The whole point of equal-power vs linear is that the sum of
    // squares stays at 1 across the fade, so total RMS doesn't dip
    // mid-window. Sample 21 points and assert the invariant.
    for i in 0..=20 {
        let t = i as f32 / 20.0;
        let (o, i_) = equal_power_gains(t);
        assert!(
            approx_eq(o * o + i_ * i_, 1.0, 1e-5),
            "energy not preserved at t={t}: {}",
            o * o + i_ * i_
        );
    }
}

#[test]
fn equal_power_clamps_out_of_range_t() {
    // t outside [0, 1] should clamp, not panic or wrap.
    assert_eq!(equal_power_gains(-0.5), equal_power_gains(0.0));
    assert_eq!(equal_power_gains(1.5), equal_power_gains(1.0));
}
