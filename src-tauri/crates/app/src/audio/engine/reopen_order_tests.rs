use super::rebuild::must_release_before_reopening;

#[test]
fn nothing_to_release_when_no_stream_is_installed() {
    assert!(!must_release_before_reopening(None, true));
    assert!(!must_release_before_reopening(None, false));
}

#[test]
fn an_exclusive_stream_is_always_released_first() {
    // It owns the device outright: no open of any kind succeeds until
    // it lets go. True in both directions and on every platform.
    assert!(must_release_before_reopening(Some(true), true));
    assert!(must_release_before_reopening(Some(true), false));
}

#[test]
fn entering_exclusive_over_a_shared_stream_depends_on_the_platform() {
    // A shared client blocks nothing on Windows (measured: an
    // exclusive `Initialize` succeeds alongside one) and Linux asks
    // the sound server for the card, so both keep the spawn-first
    // order and the rollback it buys. macOS records hog mode against
    // a pid and would be asked to evict this very process, so it
    // cannot.
    assert_eq!(
        must_release_before_reopening(Some(false), true),
        cfg!(target_os = "macos")
    );
}

#[test]
fn shared_to_shared_has_nothing_to_reorder() {
    assert!(!must_release_before_reopening(Some(false), false));
}
