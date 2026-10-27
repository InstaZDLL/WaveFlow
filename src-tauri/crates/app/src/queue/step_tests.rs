use super::{stepped_index, Direction, RepeatMode};

#[test]
fn next_stops_at_the_end_with_repeat_off() {
    assert_eq!(
        stepped_index(3, 2, Direction::Next, RepeatMode::Off),
        None,
        "the step runs off the end, so there is nothing to commit"
    );
    assert_eq!(
        stepped_index(3, 1, Direction::Next, RepeatMode::Off),
        Some(2)
    );
}

#[test]
fn repeat_all_wraps_both_ways() {
    assert_eq!(
        stepped_index(3, 2, Direction::Next, RepeatMode::All),
        Some(0)
    );
    assert_eq!(
        stepped_index(3, 0, Direction::Previous, RepeatMode::All),
        Some(2)
    );
}

#[test]
fn repeat_one_stays_put_whichever_way_you_step() {
    assert_eq!(
        stepped_index(3, 1, Direction::Next, RepeatMode::One),
        Some(1)
    );
    assert_eq!(
        stepped_index(3, 1, Direction::Previous, RepeatMode::One),
        Some(1)
    );
}

#[test]
fn previous_clamps_at_the_start_with_repeat_off() {
    assert_eq!(
        stepped_index(3, 0, Direction::Previous, RepeatMode::Off),
        Some(0)
    );
}

#[test]
fn a_cursor_past_the_end_steps_from_inside_the_queue() {
    // `queue.current_index` can outlive the rows it pointed at — a
    // queue that shrank under it. Stepping from there would otherwise
    // read a row that no longer exists.
    assert_eq!(
        stepped_index(3, 99, Direction::Next, RepeatMode::All),
        Some(0)
    );
    assert_eq!(
        stepped_index(3, 99, Direction::Previous, RepeatMode::Off),
        Some(1)
    );
}

#[test]
fn an_empty_queue_has_nowhere_to_step() {
    assert_eq!(stepped_index(0, 0, Direction::Next, RepeatMode::All), None);
}
