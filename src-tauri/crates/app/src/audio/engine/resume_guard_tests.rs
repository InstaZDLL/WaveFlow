use std::sync::atomic::{AtomicBool, Ordering};

use super::ResumeGuard;

#[test]
fn the_slot_is_taken_once_and_released_on_drop() {
    let slot = AtomicBool::new(false);
    // The first Play claims it.
    assert!(!slot.swap(true, Ordering::AcqRel));
    {
        let _guard = ResumeGuard(&slot);
        // A second Play landing while the first resume is still
        // awaiting the database finds the slot taken and backs off.
        assert!(slot.swap(true, Ordering::AcqRel));
    }
    // Released on drop, including the `?` paths inside `resume_last`:
    // a leaked slot would leave Play dead for the whole session.
    assert!(!slot.load(Ordering::Acquire));
}
