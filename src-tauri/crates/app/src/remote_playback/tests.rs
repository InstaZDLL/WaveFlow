use super::*;

#[test]
fn clear_if_undoes_only_its_own_install() {
    let playback = RemotePlayback::default();
    let mine = playback.set(RemoteQueue {
        entries: vec![entry("a")],
        index: 0,
    });
    assert!(playback.clear_if(mine), "its own install is undone");
    assert!(!playback.is_active());
}

#[test]
fn clear_if_leaves_a_session_the_user_navigated_alone() {
    // The start that installed the session is rolling back because its
    // own load was superseded — by a jump inside that very session.
    // Its queue is the one playing, so the rollback must not fire
    // (#622). Counting installs alone would have missed this: a
    // navigation does not install anything.
    let playback = RemotePlayback::default();
    let installed = playback.set(RemoteQueue {
        entries: vec![entry("a"), entry("b")],
        index: 0,
    });
    assert_eq!(playback.seek_to(1).map(|e| e.id).as_deref(), Some("b"));
    assert!(
        !playback.clear_if(installed),
        "the jump made the session the navigator's"
    );
    assert!(playback.is_active(), "the queue the user can hear survives");
    assert_eq!(playback.current().map(|e| e.id).as_deref(), Some("b"));
}

#[test]
fn clear_if_leaves_a_session_that_was_stepped_alone() {
    // Same through `step`, which is the auto-advance's path.
    let playback = RemotePlayback::default();
    let installed = playback.set(RemoteQueue {
        entries: vec![entry("a"), entry("b")],
        index: 0,
    });
    assert_eq!(
        playback
            .step(Direction::Next, RepeatMode::Off)
            .map(|e| e.id)
            .as_deref(),
        Some("b")
    );
    assert!(!playback.clear_if(installed));
    assert!(playback.is_active());
}

#[test]
fn clear_if_leaves_a_newer_session_alone() {
    // A remote start whose load was superseded rolls back, but by then
    // the session in place belongs to whoever came after it. Dropping
    // that one would strand the queue the user is listening to (#622).
    let playback = RemotePlayback::default();
    let stale = playback.set(RemoteQueue {
        entries: vec![entry("a")],
        index: 0,
    });
    let _newer = playback.set(RemoteQueue {
        entries: vec![entry("b")],
        index: 0,
    });
    assert!(!playback.clear_if(stale), "nothing of ours left to undo");
    assert_eq!(playback.current().map(|e| e.id).as_deref(), Some("b"));
}

#[test]
fn next_off_stops_at_the_end() {
    assert_eq!(
        advance_index(3, 0, Direction::Next, RepeatMode::Off),
        Some(1)
    );
    assert_eq!(
        advance_index(3, 1, Direction::Next, RepeatMode::Off),
        Some(2)
    );
    assert_eq!(advance_index(3, 2, Direction::Next, RepeatMode::Off), None);
}

#[test]
fn next_all_wraps() {
    assert_eq!(
        advance_index(3, 2, Direction::Next, RepeatMode::All),
        Some(0)
    );
}

#[test]
fn previous_off_clamps_at_zero() {
    assert_eq!(
        advance_index(3, 0, Direction::Previous, RepeatMode::Off),
        Some(0)
    );
    assert_eq!(
        advance_index(3, 2, Direction::Previous, RepeatMode::Off),
        Some(1)
    );
}

#[test]
fn previous_all_wraps_to_end() {
    assert_eq!(
        advance_index(3, 0, Direction::Previous, RepeatMode::All),
        Some(2)
    );
}

#[test]
fn repeat_one_holds_the_slot() {
    assert_eq!(
        advance_index(3, 1, Direction::Next, RepeatMode::One),
        Some(1)
    );
    assert_eq!(
        advance_index(3, 1, Direction::Previous, RepeatMode::One),
        Some(1)
    );
}

#[test]
fn an_empty_queue_never_advances() {
    assert_eq!(advance_index(0, 0, Direction::Next, RepeatMode::All), None);
}

#[test]
fn step_clears_when_it_runs_off_the_end() {
    let playback = RemotePlayback::default();
    let _ = playback.set(RemoteQueue {
        entries: vec![entry("a"), entry("b")],
        index: 1,
    });
    assert!(playback.is_active());
    // Next past the last entry with repeat off ends the session.
    assert!(playback.step(Direction::Next, RepeatMode::Off).is_none());
    assert!(!playback.is_active());
}

#[test]
fn step_advances_and_reports_the_landing_entry() {
    let playback = RemotePlayback::default();
    let _ = playback.set(RemoteQueue {
        entries: vec![entry("a"), entry("b"), entry("c")],
        index: 0,
    });
    let landed = playback.step(Direction::Next, RepeatMode::Off).unwrap();
    assert_eq!(landed.id, "b");
    assert_eq!(playback.current().unwrap().id, "b");
}

fn entry(id: &str) -> RemoteEntry {
    RemoteEntry {
        id: id.into(),
        title: None,
        artist: None,
        artist_id: None,
        artwork_hash: None,
        duration_ms: None,
    }
}
