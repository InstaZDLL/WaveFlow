use super::remove::cursor_after_removal;
use super::shuffle::{album_runs, QueuedAlbumTrack};
use super::*;

#[test]
fn a_position_in_the_last_two_seconds_starts_the_track_over() {
    use super::resume_position;
    assert_eq!(resume_position(210_000, 211_000), 0);
    assert_eq!(resume_position(211_000, 211_000), 0);
    assert_eq!(resume_position(60_000, 211_000), 60_000);
    // An unknown duration leaves the stored position alone.
    assert_eq!(resume_position(60_000, 0), 60_000);
}

// ------------------------------------------------------------------
// Album shuffle (#618)
// ------------------------------------------------------------------

/// `(track_id, position, album_id, disc, track_no)` — the shape
/// the queue query hands [`album_runs`].
fn queued(
    id: i64,
    position: i64,
    album: Option<i64>,
    disc: Option<i64>,
    track_no: Option<i64>,
) -> QueuedAlbumTrack {
    (id, position, album, disc, track_no)
}

/// Two records interleaved in the queue come back as two blocks,
/// each in its own disc/track order rather than the order the
/// queue happened to hold.
#[test]
fn album_runs_group_records_and_restore_their_order() {
    let rows = vec![
        queued(30, 0, Some(1), Some(1), Some(3)),
        queued(20, 1, Some(2), Some(1), Some(2)),
        queued(10, 2, Some(1), Some(1), Some(1)),
        queued(21, 3, Some(2), Some(1), Some(1)),
        queued(11, 4, Some(1), Some(1), Some(2)),
    ];
    let runs = album_runs(&rows, 10);
    assert_eq!(runs.len(), 2, "two albums, two runs");
    assert_eq!(runs[0], vec![10, 11, 30], "album 1 in track order");
    assert!(
        runs[1..].iter().any(|r| *r == vec![21, 20]),
        "album 2 in track order, got {runs:?}"
    );
}

/// The record being listened to comes first, and carries on from
/// the current track rather than restarting: a rotation, so the
/// tracks already behind you come back round at the end instead of
/// vanishing from the queue.
#[test]
fn the_current_record_continues_from_where_it_is() {
    let rows = vec![
        queued(1, 0, Some(7), Some(1), Some(1)),
        queued(2, 1, Some(7), Some(1), Some(2)),
        queued(3, 2, Some(7), Some(1), Some(3)),
        queued(4, 3, Some(7), Some(1), Some(4)),
        queued(99, 4, Some(8), Some(1), Some(1)),
    ];
    let runs = album_runs(&rows, 3);
    assert_eq!(runs[0], vec![3, 4, 1, 2]);
    assert_eq!(runs[0][0], 3, "the current track keeps position 0");
}

/// A shuffle is a permutation. Whatever the grouping does, every
/// track that went in comes back out exactly once — the bug that
/// would quietly shorten someone's queue.
#[test]
fn every_track_survives_the_grouping_exactly_once() {
    let rows = vec![
        queued(1, 0, Some(7), Some(1), Some(2)),
        queued(2, 1, None, None, None),
        queued(3, 2, Some(7), Some(2), Some(1)),
        queued(4, 3, Some(8), None, None),
        queued(5, 4, None, None, None),
        queued(6, 5, Some(7), Some(1), Some(1)),
    ];
    let mut seen: Vec<i64> = album_runs(&rows, 3).into_iter().flatten().collect();
    seen.sort_unstable();
    assert_eq!(seen, vec![1, 2, 3, 4, 5, 6]);
}

/// A track with no album is its own record. Grouping every NULL
/// together would weld unrelated loose files into one block that
/// always plays in the same order — the opposite of shuffling.
#[test]
fn tracks_with_no_album_are_each_their_own_record() {
    let rows = vec![
        queued(1, 0, None, None, None),
        queued(2, 1, None, None, None),
        queued(3, 2, None, None, None),
    ];
    let runs = album_runs(&rows, 1);
    assert_eq!(runs.len(), 3, "three loose files, three runs");
    assert!(runs.iter().all(|r| r.len() == 1));
}

/// Disc order beats track order, and an untagged track sorts after
/// every numbered one — a bonus track with no number belongs at
/// the end of the record, not in front of track 1.
#[test]
fn discs_come_in_order_and_untagged_tracks_go_last() {
    let rows = vec![
        queued(40, 0, Some(1), None, None),
        queued(21, 1, Some(1), Some(2), Some(1)),
        queued(12, 2, Some(1), Some(1), Some(2)),
        queued(11, 3, Some(1), Some(1), Some(1)),
        queued(41, 4, Some(1), None, None),
    ];
    let runs = album_runs(&rows, 11);
    assert_eq!(runs.len(), 1);
    // Rotated onto the current track, which is already first here.
    assert_eq!(runs[0], vec![11, 12, 21, 40, 41]);
}

/// Two untagged tracks keep the order the queue already had, so the
/// result does not depend on how the rows came back.
#[test]
fn ties_fall_back_to_the_position_the_queue_already_had() {
    let rows = vec![
        queued(50, 3, Some(1), None, None),
        queued(51, 1, Some(1), None, None),
        queued(52, 2, Some(1), None, None),
    ];
    let runs = album_runs(&rows, 51);
    assert_eq!(runs[0], vec![51, 52, 50]);
}

/// The current track not being in the queue at all should not
/// panic or drop a record — the first run is simply whichever came
/// first.
#[test]
fn an_absent_current_track_still_yields_every_run() {
    let rows = vec![
        queued(1, 0, Some(7), Some(1), Some(1)),
        queued(2, 1, Some(8), Some(1), Some(1)),
    ];
    let runs = album_runs(&rows, 999);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0], vec![1]);
}

/// The mode is persisted as a string other surfaces read back.
#[test]
fn shuffle_mode_round_trips_through_string() {
    for mode in [ShuffleMode::Off, ShuffleMode::Tracks, ShuffleMode::Albums] {
        assert_eq!(ShuffleMode::from_str(mode.as_str()), mode);
    }
    for junk in ["", "album", "ALBUMS", "true"] {
        assert_eq!(ShuffleMode::from_str(junk), ShuffleMode::Off);
    }
    assert!(!ShuffleMode::Off.is_on());
    assert!(ShuffleMode::Tracks.is_on());
    assert!(ShuffleMode::Albums.is_on());
}

#[test]
fn repeat_mode_round_trips_through_string() {
    for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
        assert_eq!(RepeatMode::from_str(mode.as_str()), mode);
    }
}

#[test]
fn repeat_mode_unknown_string_falls_back_to_off() {
    // The DB stores user-facing strings; an old / corrupted value
    // must not panic. "Off" is the conservative default.
    assert_eq!(RepeatMode::from_str("garbage"), RepeatMode::Off);
    assert_eq!(RepeatMode::from_str(""), RepeatMode::Off);
}

#[test]
fn removing_before_the_cursor_keeps_the_same_track() {
    // Queue [A,B,C,D] len 4, cursor 2 (C is playing).
    // Delete [0,1) (A) → C is now at position 1, so the cursor follows.
    assert_eq!(cursor_after_removal(2, 0, 1, 1, 3), 1);
    // Delete [0,2) (A,B) → C is now at position 0.
    assert_eq!(cursor_after_removal(2, 0, 2, 2, 2), 0);
}

#[test]
fn removing_at_or_after_the_cursor_behaves() {
    // Delete after the cursor → the cursor doesn't move.
    assert_eq!(cursor_after_removal(1, 2, 4, 2, 2), 1);
    // Delete a span that includes the playing track → land on `start`,
    // whatever fell into the gap.
    assert_eq!(cursor_after_removal(2, 1, 3, 2, 2), 1);
    // Delete the whole queue → clamp to 0.
    assert_eq!(cursor_after_removal(0, 0, 4, 4, 0), 0);
}
