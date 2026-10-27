use super::*;

const SETTLE: Duration = REBUILD_SETTLE_WINDOW;

#[test]
fn arms_the_first_device_error() {
    let mut g = RebuildGate::default();
    assert!(g.try_arm(Instant::now(), SETTLE));
}

#[test]
fn refuses_a_second_arm_while_one_is_still_armed() {
    let mut g = RebuildGate::default();
    let t = Instant::now();
    assert!(g.try_arm(t, SETTLE));
    // The deferred rebuild hasn't run yet: every further error in the
    // burst must be swallowed, however far apart they land.
    assert!(!g.try_arm(t + Duration::from_millis(10), SETTLE));
    assert!(!g.try_arm(t + Duration::from_secs(30), SETTLE));
}

#[test]
fn refuses_a_rearm_inside_the_settle_window() {
    let mut g = RebuildGate::default();
    let t = Instant::now();
    assert!(g.try_arm(t, SETTLE));
    g.finish(t + Duration::from_millis(300));
    // This is the #365 cascade: our own exclusive re-open makes the
    // DAC reset, and the resulting error must NOT arm another pass.
    assert!(!g.try_arm(t + Duration::from_millis(340), SETTLE));
}

#[test]
fn arms_again_once_the_settle_window_has_passed() {
    let mut g = RebuildGate::default();
    let t = Instant::now();
    assert!(g.try_arm(t, SETTLE));
    let finished = t + Duration::from_millis(300);
    g.finish(finished);
    assert!(!g.try_arm(finished + SETTLE - Duration::from_millis(1), SETTLE));
    // A genuine later failure (real unplug) still recovers.
    assert!(g.try_arm(finished + SETTLE, SETTLE));
}

#[test]
fn finish_reopens_the_gate_even_when_the_rebuild_failed() {
    let mut g = RebuildGate::default();
    let t = Instant::now();
    assert!(g.try_arm(t, SETTLE));
    // `finish` runs from the RAII guard on every exit path, including
    // the error ones — otherwise `armed` would latch forever and the
    // engine could never recover from a later device loss.
    g.finish(t);
    assert!(g.try_arm(t + SETTLE, SETTLE));
}

#[test]
fn reopen_lets_a_rebuild_arm_inside_the_settle_window() {
    let mut g = RebuildGate::default();
    let t = Instant::now();
    // A deliberate output change opens the settle window...
    g.finish(t);
    // ...but the spawn failed and installed nothing, so we reopen.
    g.reopen();
    // The scheduled recovery lands 300 ms later — well inside what
    // would have been the 2 s window — and MUST now arm, otherwise
    // the engine is left with no output thread at all (#405).
    assert!(g.try_arm(t + Duration::from_millis(300), SETTLE));
}

#[test]
fn collapses_a_full_cascade_into_a_single_rebuild() {
    // Replays the reporter's pattern: an error every ~300 ms, each one
    // queueing its own deferred recovery task. Mirrors the real call
    // order — a task arms only after its 300 ms backoff, so the gate
    // is consulted at `error + 300 ms`, not at error time.
    let mut g = RebuildGate::default();
    let start = Instant::now();
    let mut armed = 0;
    for i in 0..10 {
        let error_at = start + Duration::from_millis(300 * i);
        let arms_at = error_at + Duration::from_millis(300);
        if g.try_arm(arms_at, SETTLE) {
            armed += 1;
            // The rebuild itself takes a few tens of ms.
            g.finish(arms_at + Duration::from_millis(40));
        }
    }
    // 10 errors over 2.7 s collapse to the initial rebuild plus one
    // retry after the settle window — not one per error.
    assert_eq!(armed, 2);
}
