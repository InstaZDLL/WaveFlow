use super::{
    next_stall_retry, stall_retry_delay, StormAction, StreamErrorStorm, STALL_RETRY_MAX,
    STALL_RETRY_MIN,
};
use std::time::{Duration, Instant};

#[test]
fn a_stream_that_had_been_working_is_rebuilt_at_once() {
    assert_eq!(stall_retry_delay(0), Duration::ZERO);
}

#[test]
fn repeated_stalls_back_off_to_once_a_minute() {
    assert_eq!(stall_retry_delay(1), STALL_RETRY_MIN);
    assert_eq!(stall_retry_delay(2), STALL_RETRY_MIN * 2);
    assert_eq!(stall_retry_delay(3), STALL_RETRY_MIN * 4);
    assert_eq!(stall_retry_delay(40), STALL_RETRY_MAX);
    assert_eq!(stall_retry_delay(u32::MAX), STALL_RETRY_MAX);
}

#[test]
fn a_retry_waits_out_the_gate_and_never_passes_the_cap() {
    assert_eq!(next_stall_retry(Duration::ZERO), STALL_RETRY_MIN);
    assert_eq!(next_stall_retry(STALL_RETRY_MIN), STALL_RETRY_MIN * 2);
    assert_eq!(next_stall_retry(STALL_RETRY_MAX), STALL_RETRY_MAX);
    assert!(STALL_RETRY_MIN > crate::audio::engine::REBUILD_SETTLE_WINDOW);
}

#[test]
fn the_first_error_is_reported_and_the_next_ones_are_not() {
    let mut storm = StreamErrorStorm::default();
    let t = Instant::now();
    assert_eq!(
        storm.on_error(t, false),
        StormAction::Report { rebuild: false }
    );
    assert_eq!(
        storm.on_error(t + Duration::from_millis(400), false),
        StormAction::Quiet
    );
}

#[test]
fn a_storm_rebuilds_once() {
    let mut storm = StreamErrorStorm::default();
    let t = Instant::now();
    let actions: Vec<StormAction> = (0..1000)
        .map(|i| storm.on_error(t + Duration::from_micros(i * 10), false))
        .collect();
    assert_eq!(actions[0], StormAction::Report { rebuild: false });
    let rebuilds: Vec<usize> = actions
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a, StormAction::Rebuild { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(rebuilds, vec![StreamErrorStorm::THRESHOLD as usize - 1]);
}

#[test]
fn a_storm_that_outlasts_the_rebuild_asks_again() {
    let mut storm = StreamErrorStorm::default();
    let t = Instant::now();
    // Ten seconds of errors every millisecond: the first request may
    // be turned down by the rebuild gate, so it is repeated — at the
    // retry pace, never at the error loop's.
    let rebuilds = (0..10_000)
        .filter(|i| {
            matches!(
                storm.on_error(t + Duration::from_millis(*i), false),
                StormAction::Rebuild { .. }
            )
        })
        .count();
    let most = (10_000 / StreamErrorStorm::RETRY.as_millis()) as usize + 1;
    assert!((2..=most).contains(&rebuilds), "{rebuilds} rebuilds");
}

#[test]
fn a_device_lost_after_other_errors_still_rebuilds() {
    let mut storm = StreamErrorStorm::default();
    let t = Instant::now();
    assert_eq!(
        storm.on_error(t, false),
        StormAction::Report { rebuild: false }
    );
    assert_eq!(
        storm.on_error(t + Duration::from_millis(5), true),
        StormAction::Rebuild { errors: 2 }
    );
    assert_eq!(
        storm.on_error(t + Duration::from_millis(9), true),
        StormAction::Quiet
    );
}

#[test]
fn a_device_loss_is_asked_again_once_the_retry_delay_has_passed() {
    // The gate may have turned the first request down; a later loss on
    // the same stream must not stay quiet for good, storm or no storm.
    let mut storm = StreamErrorStorm::default();
    let t = Instant::now();
    assert_eq!(
        storm.on_error(t, true),
        StormAction::Report { rebuild: true }
    );
    assert_eq!(
        storm.on_error(t + Duration::from_millis(500), true),
        StormAction::Quiet
    );
    assert_eq!(
        storm.on_error(t + StreamErrorStorm::RETRY, true),
        StormAction::Rebuild { errors: 3 }
    );
}

#[test]
fn a_device_lost_first_is_reported_with_its_rebuild() {
    let mut storm = StreamErrorStorm::default();
    assert_eq!(
        storm.on_error(Instant::now(), true),
        StormAction::Report { rebuild: true }
    );
}

#[test]
fn scattered_errors_never_make_a_storm() {
    let mut storm = StreamErrorStorm::default();
    let t = Instant::now();
    for i in 0..200 {
        let action = storm.on_error(t + Duration::from_millis(i * 100), false);
        assert!(!matches!(action, StormAction::Rebuild { .. }), "error {i}");
    }
}
