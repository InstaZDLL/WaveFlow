use super::rebuild::EXCLUSIVE_FLAP_WINDOW;
use super::*;

// Threshold is 3 within a 12 s window — see the consts under test.

#[test]
fn trips_on_the_threshold_flap_within_window() {
    let mut w = FlapWindow::default();
    let t = Instant::now();
    assert!(!w.record(t)); // 1
    assert!(!w.record(t + Duration::from_secs(2))); // 2
    assert!(w.record(t + Duration::from_secs(4))); // 3 → trip
}

#[test]
fn resets_when_a_flap_lands_outside_the_window() {
    let mut w = FlapWindow::default();
    let t = Instant::now();
    assert!(!w.record(t)); // 1
    assert!(!w.record(t + Duration::from_secs(1))); // 2
                                                    // Past the window → counter restarts, so this does NOT trip.
    let after = t + EXCLUSIVE_FLAP_WINDOW + Duration::from_secs(1);
    assert!(!w.record(after)); // 1 (fresh window)
    assert!(!w.record(after + Duration::from_secs(1))); // 2
    assert!(w.record(after + Duration::from_secs(2))); // 3 → trip
}

#[test]
fn never_trips_for_spaced_out_flaps() {
    let mut w = FlapWindow::default();
    let mut t = Instant::now();
    // Each flap sits just past the previous window, so the count keeps
    // restarting at 1 and never reaches the threshold.
    for _ in 0..6 {
        assert!(!w.record(t));
        t += EXCLUSIVE_FLAP_WINDOW + Duration::from_secs(1);
    }
}
