use super::should_follow_default;

#[test]
fn a_new_default_is_followed() {
    assert!(should_follow_default(Some("Speakers"), Some("USB DAC")));
}

#[test]
fn the_same_default_is_not_reopened() {
    // The notification fires for changes that leave the endpoint we
    // are on as the default — and a rebuild is an audible gap, so
    // "nothing to do" has to stay nothing.
    assert!(!should_follow_default(Some("USB DAC"), Some("USB DAC")));
}

#[test]
fn an_unnamed_output_is_followed() {
    // CoreAudio hog mode with no pin reports no opened name (#612:
    // inventing one was the original defect). Unknown must not mean
    // "stay put" here.
    assert!(should_follow_default(None, Some("USB DAC")));
}

#[test]
fn a_vanished_default_is_not_followed() {
    // Windows sends the notification with no device when the last
    // endpoint goes away. Rebuilding onto nothing would drop audio
    // that is still playing; a device we really lost arrives as
    // `DeviceNotAvailable` instead, which owns its own recovery.
    assert!(!should_follow_default(Some("USB DAC"), None));
    assert!(!should_follow_default(None, None));
}

#[test]
fn the_comparison_is_exact() {
    // Both names come from the same accessor, so they are the same
    // string for the same endpoint — no normalisation to hide a
    // genuine change behind.
    assert!(should_follow_default(Some("USB DAC"), Some("USB  DAC")));
}
