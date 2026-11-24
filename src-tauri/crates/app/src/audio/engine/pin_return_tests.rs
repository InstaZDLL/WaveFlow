//! The return of a pinned device (#1165): when it is reopened, what a
//! reopen that lands elsewhere leaves to do, and when a pick of it may
//! do nothing.

use super::rebuild::{pick_is_noop, pin_reopen_outcome, should_reopen_pin, FollowOutcome};

const BUDS: &str = "Headphones (Buds2)";
const SPEAKERS: &str = "Speakers (Realtek(R) Audio)";

#[test]
fn a_returning_pin_is_reopened_from_the_fallback() {
    // The reported case: the headset dropped, the stream fell back to
    // the speakers, and the headset is listed again.
    assert!(should_reopen_pin(BUDS, Some(SPEAKERS), true));
}

#[test]
fn a_pin_still_absent_is_left_alone() {
    // Reopening would only fall back to the speakers again: an audible
    // gap for every unrelated device that comes and goes.
    assert!(!should_reopen_pin(BUDS, Some(SPEAKERS), false));
}

#[test]
fn an_output_already_on_the_pin_is_left_alone() {
    // Every notification of a Bluetooth connection reaches the check,
    // and only the first one may rebuild.
    assert!(!should_reopen_pin(BUDS, Some(BUDS), true));
}

#[test]
fn an_unnamed_output_is_reopened_onto_a_listed_pin() {
    // CoreAudio hog mode names nothing after its fallback, so unknown
    // must not read as "already on the pin".
    assert!(should_reopen_pin(BUDS, None, true));
    assert!(!should_reopen_pin(BUDS, None, false));
}

#[test]
fn the_pin_comparison_is_exact() {
    assert!(should_reopen_pin(BUDS, Some("Headphones (Buds2) "), true));
}

#[test]
fn a_reopen_that_reached_the_pin_settles() {
    assert_eq!(pin_reopen_outcome(BUDS, Some(BUDS)), FollowOutcome::Settled);
}

#[test]
fn a_reopen_that_fell_back_again_is_retried() {
    // A Bluetooth endpoint can be listed a moment before it opens, and
    // nothing will be sent once it does.
    assert_eq!(
        pin_reopen_outcome(BUDS, Some(SPEAKERS)),
        FollowOutcome::Deferred
    );
}

#[test]
fn a_reopen_onto_an_unnamed_output_settles() {
    // It cannot be told from the pin, and retrying would reopen the
    // stream three times for what may be the right device.
    assert_eq!(pin_reopen_outcome(BUDS, None), FollowOutcome::Settled);
}

#[test]
fn picking_the_pin_after_a_fallback_reopens_it() {
    // The handle still asks for the pin while the speakers play: the
    // click that did nothing in the report.
    assert!(!pick_is_noop(true, Some(BUDS), Some(SPEAKERS), Some(BUDS)));
}

#[test]
fn picking_the_device_that_plays_is_a_noop() {
    assert!(pick_is_noop(true, Some(BUDS), Some(BUDS), Some(BUDS)));
}

#[test]
fn picking_another_device_is_a_switch() {
    assert!(!pick_is_noop(true, Some(BUDS), Some(BUDS), Some(SPEAKERS)));
    assert!(!pick_is_noop(true, None, Some(SPEAKERS), Some(SPEAKERS)));
    assert!(!pick_is_noop(true, Some(BUDS), Some(SPEAKERS), None));
}

#[test]
fn any_pick_without_a_stream_reaches_the_rebuild() {
    // #612 / #630: with no stream installed, the pick is the way back.
    assert!(!pick_is_noop(false, Some(BUDS), None, Some(BUDS)));
    assert!(!pick_is_noop(false, None, None, None));
}

#[test]
fn an_unnamed_output_keeps_the_noop() {
    // The picker ticks the pinned row then, and its click goes to the
    // reopen command instead.
    assert!(pick_is_noop(true, Some(BUDS), None, Some(BUDS)));
}

#[test]
fn the_os_default_is_judged_on_the_request() {
    // Following the default is the default-device follow's job.
    assert!(pick_is_noop(true, None, Some(SPEAKERS), None));
    assert!(pick_is_noop(true, None, None, None));
}
