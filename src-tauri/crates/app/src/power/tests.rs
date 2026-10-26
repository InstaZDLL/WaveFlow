use super::{wanted, Hold};

#[test]
fn nothing_is_held_while_paused() {
    for enabled in [false, true] {
        for display in [false, true] {
            for seen in [false, true] {
                assert_eq!(wanted(false, enabled, display, seen), None);
            }
        }
    }
}

#[test]
fn the_setting_keeps_the_system_awake_while_playing() {
    assert_eq!(wanted(true, true, false, true), Some(Hold::System));
    assert_eq!(wanted(true, false, false, true), None);
}

#[test]
fn the_theater_view_holds_the_display_whatever_the_setting() {
    assert_eq!(wanted(true, true, true, true), Some(Hold::Display));
    assert_eq!(wanted(true, false, true, true), Some(Hold::Display));
}

#[test]
fn a_window_out_of_sight_holds_no_display() {
    // Back to what the setting asks for.
    assert_eq!(wanted(true, true, true, false), Some(Hold::System));
    assert_eq!(wanted(true, false, true, false), None);
}
