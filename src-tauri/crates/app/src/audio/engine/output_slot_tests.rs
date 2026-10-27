use super::OutputSlot;

// Only the handle-less branch is exercised here: an `OutputHandle`
// owns a live output thread, so it cannot be built in a unit test.
// That branch is the one that regressed, though — the other reads the
// handle the rebuilds keep in step with the stream.

#[test]
fn the_pin_survives_a_missing_handle() {
    // #630: with no stream installed — a spawn that failed after the
    // old one was released — the engine must still name the device the
    // user picked. Answering "nothing pinned" sent a forced reopen to
    // the OS default, cost the picker the pinned row it had just
    // failed to open, and made MPD report no device at all.
    let slot = OutputSlot {
        handle: None,
        pinned: Some("Speakers (USB DAC)".to_string()),
    };
    assert_eq!(slot.pinned_device().as_deref(), Some("Speakers (USB DAC)"));
}

#[test]
fn no_handle_and_no_pin_means_the_os_default() {
    let slot = OutputSlot {
        handle: None,
        pinned: None,
    };
    assert!(slot.pinned_device().is_none());
}
