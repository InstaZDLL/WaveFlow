use super::remember;

// Each test uses its own label: the cache is process-wide.

#[test]
fn a_window_never_shown_is_not_lowered() {
    assert!(!remember("t-never-shown", Some(true)));
    assert!(remember("t-never-shown", Some(false)));
}

#[test]
fn a_shown_window_is_lowered_once_then_restored() {
    assert!(remember("t-cycle", Some(false)));
    assert!(remember("t-cycle", Some(true)));
    assert!(!remember("t-cycle", Some(true)), "same level is not resent");
    assert!(remember("t-cycle", Some(false)));
}

#[test]
fn a_failed_call_is_retried_by_the_next_event() {
    assert!(remember("t-retry", Some(false)));
    remember("t-retry", None);
    assert!(remember("t-retry", Some(false)));
}

#[test]
fn a_failed_lowering_is_retried_on_the_next_hide() {
    assert!(remember("t-retry-low", Some(false)));
    assert!(remember("t-retry-low", Some(true)));
    remember("t-retry-low", None);
    assert!(
        remember("t-retry-low", Some(true)),
        "a failure must not make the window count as never shown"
    );
}
