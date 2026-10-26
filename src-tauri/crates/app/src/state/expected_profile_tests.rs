use super::*;

#[test]
fn no_expectation_always_passes() {
    assert!(check_expected_profile(Some(1), None).is_ok());
    assert!(check_expected_profile(None, None).is_ok());
}

#[test]
fn matching_expectation_passes() {
    assert!(check_expected_profile(Some(7), Some(7)).is_ok());
}

#[test]
fn switched_profile_is_refused() {
    let err = check_expected_profile(Some(2), Some(1)).unwrap_err();
    assert!(
        matches!(
            err,
            AppError::ProfileChanged {
                expected: 1,
                active: Some(2)
            }
        ),
        "unexpected error: {err}",
    );
}

#[test]
fn deactivated_profile_is_refused() {
    // `deactivate_profile` leaves no active profile; a queued write
    // that expected one must not fall through to `NoActiveProfile`
    // silently succeeding somewhere else.
    let err = check_expected_profile(None, Some(1)).unwrap_err();
    assert!(
        matches!(
            err,
            AppError::ProfileChanged {
                expected: 1,
                active: None
            }
        ),
        "unexpected error: {err}",
    );
}
