use super::{
    append_to_user_queue, apply_shuffle_mode, current_track, fill_queue, insert_after_current,
    peek_next, peek_step, read_album_ordered, write_setting_i64, Direction, RepeatMode,
    ShuffleMode, UserQueue,
};
use sqlx::SqlitePool;

/// The repo's own profile migrations, with `foreign_keys` on —
/// `fill_queue` writes rows that reference `track`, and the setting
/// it stores is written by a query nothing checks at compile time.
async fn migrated_pool() -> SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let options = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at)
                  VALUES (1, 'l', 0, 0);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'A', 1000, 0),
                         (2, 1, '/l/b.flac', 'h2', 1, 0, 'B', 1000, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// Clearing it matters as much as setting it (#647): a flag left
/// raised would hand album gain to the next ordinary playlist, and
/// the whole reason this lives in `fill_queue` is that every queue
/// replacement passes through here.
#[tokio::test]
async fn the_queue_records_whether_it_was_built_from_records() {
    let pool = migrated_pool().await;

    // Nothing stored yet is not "album-ordered".
    assert!(!read_album_ordered(&pool).await);

    fill_queue(&pool, "radio", None, &[1, 2], 0, true, UserQueue::Drop)
        .await
        .unwrap();
    assert!(read_album_ordered(&pool).await);

    fill_queue(&pool, "playlist", Some(7), &[1], 0, false, UserQueue::Drop)
        .await
        .unwrap();
    assert!(
        !read_album_ordered(&pool).await,
        "the next queue is not a session of records because the last one was"
    );
}

/// `fill_queue` is not the only path that builds a queue where
/// there was none, which is exactly how a flag survives the session
/// it described: both "Play next" and "Add to queue" fill an empty
/// queue themselves, the second one inline.
#[tokio::test]
async fn the_hand_built_queues_clear_it_too() {
    for (label, replaced) in [("play next", true), ("add to queue", false)] {
        let pool = migrated_pool().await;
        fill_queue(&pool, "radio", None, &[1, 2], 0, true, UserQueue::Drop)
            .await
            .unwrap();
        // Emptied the way a removal would leave it.
        sqlx::query("DELETE FROM queue_item")
            .execute(&pool)
            .await
            .unwrap();

        let was_replaced = if replaced {
            insert_after_current(&pool, &[1], "manual", None)
                .await
                .unwrap()
        } else {
            append_to_user_queue(&pool, &[1], None).await.unwrap()
        };

        assert!(was_replaced, "{label} filled an empty queue");
        assert!(
            !read_album_ordered(&pool).await,
            "{label} built a queue by hand, so the old session's flag is gone"
        );
    }
}

/// And neither touches it when there is a queue to insert into:
/// adding a track to a record playing through does not stop it
/// being one.
#[tokio::test]
async fn inserting_into_a_live_queue_leaves_the_session_alone() {
    let pool = migrated_pool().await;
    fill_queue(&pool, "radio", None, &[1, 2], 0, true, UserQueue::Drop)
        .await
        .unwrap();

    assert!(!insert_after_current(&pool, &[2], "manual", None)
        .await
        .unwrap());
    assert!(!append_to_user_queue(&pool, &[2], None).await.unwrap());
    assert!(read_album_ordered(&pool).await);
}

/// A cursor left past the end — the queue shrank under it — is read
/// as the last row, as a step reads it: the playing track is still
/// reported, and what is preloaded next is what the step will play.
#[tokio::test]
async fn a_stale_cursor_is_read_inside_the_queue() {
    let pool = migrated_pool().await;
    fill_queue(
        &pool,
        "playlist",
        Some(1),
        &[1, 2],
        0,
        false,
        UserQueue::Drop,
    )
    .await
    .unwrap();
    write_setting_i64(&pool, "queue.current_index", 99)
        .await
        .unwrap();

    let current = current_track(&pool)
        .await
        .unwrap()
        .expect("a current track");
    assert_eq!(current.id, 2, "the last row stands in for the stale cursor");

    for repeat in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
        let preloaded = peek_next(&pool, repeat).await.unwrap().map(|t| t.id);
        let stepped = peek_step(&pool, Direction::Next, repeat)
            .await
            .unwrap()
            .map(|(_, t)| t.id);
        assert_eq!(preloaded, stepped, "{repeat:?}: preload and step agree");
    }
}

// ------------------------------------------------------------------
// The user queue survives a fill (#1168)
// ------------------------------------------------------------------

/// Tracks 1..=12, enough for every list below.
async fn pool_with_tracks() -> SqlitePool {
    let pool = migrated_pool().await;
    for id in 3..=12 {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (?, 1, ?, ?, 1, 0, ?, 1000, 0)",
        )
        .bind(id)
        .bind(format!("/l/{id}.flac"))
        .bind(format!("h{id}"))
        .bind(format!("T{id}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    pool
}

/// The queue in order, as `(track_id, source_type, queued_by_hand)`.
async fn rows(pool: &SqlitePool) -> Vec<(i64, String, bool)> {
    sqlx::query_as("SELECT track_id, source_type, queued_by_hand FROM queue_item ORDER BY position")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn track_order(pool: &SqlitePool) -> Vec<i64> {
    rows(pool).await.into_iter().map(|(id, ..)| id).collect()
}

fn manual(id: i64) -> (i64, String, bool) {
    (id, "manual".to_string(), true)
}

/// An album playing from its first track, with 7 and 8 added to the
/// queue: `[1, 7, 8, 2, 3]`, cursor on 1.
async fn album_with_user_queue(pool: &SqlitePool) {
    fill_queue(
        pool,
        "album",
        Some(1),
        &[1, 2, 3],
        0,
        false,
        UserQueue::Drop,
    )
    .await
    .unwrap();
    append_to_user_queue(pool, &[7, 8], None).await.unwrap();
    assert_eq!(track_order(pool).await, vec![1, 7, 8, 2, 3]);
}

/// The report: queue two tracks, start another list, and the two are
/// still next — after the track that starts, before the rest of its
/// list, in the order they were queued, still marked as queued by hand.
#[tokio::test]
async fn starting_another_list_keeps_the_user_queue_after_its_first_track() {
    let pool = pool_with_tracks().await;
    album_with_user_queue(&pool).await;

    fill_queue(
        &pool,
        "playlist",
        Some(4),
        &[4, 5, 6],
        1,
        false,
        UserQueue::Keep,
    )
    .await
    .unwrap();

    let playlist = |id: i64| (id, "playlist".to_string(), false);
    assert_eq!(
        rows(&pool).await,
        vec![playlist(4), playlist(5), manual(7), manual(8), playlist(6)]
    );
    assert_eq!(
        super::current_index(&pool).await,
        1,
        "the clicked track plays"
    );
    let next = peek_next(&pool, RepeatMode::Off).await.unwrap().unwrap();
    assert_eq!(next.id, 7, "the user queue plays next");
}

/// Only what is still to come: a queued track already played, or
/// playing now, went with the old queue.
#[tokio::test]
async fn the_user_queue_already_played_does_not_survive() {
    for (cursor, kept) in [(1, vec![8]), (2, vec![]), (3, vec![])] {
        let pool = pool_with_tracks().await;
        album_with_user_queue(&pool).await;
        write_setting_i64(&pool, "queue.current_index", cursor)
            .await
            .unwrap();

        fill_queue(&pool, "album", Some(2), &[4, 5], 0, false, UserQueue::Keep)
            .await
            .unwrap();

        let mut expected = vec![4];
        expected.extend(&kept);
        expected.push(5);
        assert_eq!(track_order(&pool).await, expected, "cursor at {cursor}");
    }
}

/// Starting a track that is waiting in the user queue plays that copy
/// now; it does not also wait behind itself. A track queued twice was
/// asked for twice, so one copy stays.
#[tokio::test]
async fn the_track_started_is_taken_out_of_the_user_queue_once() {
    let pool = pool_with_tracks().await;
    fill_queue(&pool, "album", Some(1), &[1, 2], 0, false, UserQueue::Drop)
        .await
        .unwrap();
    append_to_user_queue(&pool, &[7, 8, 7], None).await.unwrap();

    fill_queue(
        &pool,
        "library",
        None,
        &[9, 7, 10],
        1,
        false,
        UserQueue::Keep,
    )
    .await
    .unwrap();

    assert_eq!(track_order(&pool).await, vec![9, 7, 8, 7, 10]);
    assert_eq!(super::current_index(&pool).await, 1);
    let marks: Vec<bool> = rows(&pool).await.into_iter().map(|r| r.2).collect();
    assert_eq!(marks, vec![false, false, true, true, false]);
}

/// "Play next" is queued by hand as much as "Add to queue", and both
/// survive in their own order: the one played next first.
#[tokio::test]
async fn play_next_is_part_of_the_user_queue() {
    let pool = pool_with_tracks().await;
    album_with_user_queue(&pool).await;
    insert_after_current(&pool, &[9], "manual", None)
        .await
        .unwrap();

    fill_queue(&pool, "album", Some(2), &[4, 5], 0, false, UserQueue::Keep)
        .await
        .unwrap();

    assert_eq!(track_order(&pool).await, vec![4, 9, 7, 8, 5]);
}

/// A selection, the history or files opened from the system fill the
/// queue as `'manual'` too. That list is a context like an album: the
/// next list replaces it, and "Add to queue" goes ahead of its rest.
#[tokio::test]
async fn a_list_played_as_manual_is_not_a_user_queue() {
    let pool = pool_with_tracks().await;
    fill_queue(&pool, "manual", None, &[1, 2, 3], 0, false, UserQueue::Keep)
        .await
        .unwrap();
    append_to_user_queue(&pool, &[7], None).await.unwrap();
    assert_eq!(
        track_order(&pool).await,
        vec![1, 7, 2, 3],
        "the queued track goes ahead of the rest of the selection"
    );

    fill_queue(&pool, "album", Some(2), &[4, 5], 0, false, UserQueue::Keep)
        .await
        .unwrap();
    assert_eq!(track_order(&pool).await, vec![4, 7, 5]);
}

/// A queue handed over by another player is the whole queue.
#[tokio::test]
async fn a_fill_that_drops_the_user_queue_is_the_whole_queue() {
    let pool = pool_with_tracks().await;
    album_with_user_queue(&pool).await;

    fill_queue(&pool, "manual", None, &[4, 5], 0, false, UserQueue::Drop)
        .await
        .unwrap();

    assert_eq!(track_order(&pool).await, vec![4, 5]);
}

/// What the new list is made of describes the new list: a radio of
/// whole records stays one with the user queue wedged in, and the
/// wedged rows still say `'manual'`, which the gain never reads as a
/// record playing through.
#[tokio::test]
async fn the_album_ordering_describes_the_new_list() {
    let pool = pool_with_tracks().await;
    album_with_user_queue(&pool).await;

    fill_queue(&pool, "radio", None, &[4, 5], 0, true, UserQueue::Keep)
        .await
        .unwrap();

    assert!(read_album_ordered(&pool).await);
    let wedged: Vec<_> = rows(&pool).await.into_iter().filter(|r| r.2).collect();
    assert_eq!(wedged, vec![manual(7), manual(8)]);
}

/// Shuffling leaves the user queue right after the current track, in
/// its order, and mixes only the rest — which is what lets a list
/// started with shuffle on keep it next. Both groupings; and turning
/// shuffle off puts everything back, the rows' marks with them.
#[tokio::test]
async fn shuffle_leaves_the_user_queue_in_front() {
    for mode in [ShuffleMode::Tracks, ShuffleMode::Albums] {
        let pool = pool_with_tracks().await;
        album_with_user_queue(&pool).await;

        // Started with shuffle on: fill, then shuffle, as the player does.
        fill_queue(
            &pool,
            "playlist",
            Some(4),
            &[4, 5, 6, 9, 10, 11, 12],
            2,
            false,
            UserQueue::Keep,
        )
        .await
        .unwrap();
        let before = rows(&pool).await;
        apply_shuffle_mode(&pool, mode).await.unwrap();

        let after = rows(&pool).await;
        assert_eq!(super::current_index(&pool).await, 0, "{mode:?}");
        assert_eq!(after[0].0, 6, "{mode:?}: the clicked track plays");
        assert_eq!(
            after[1..3].to_vec(),
            vec![manual(7), manual(8)],
            "{mode:?}: the user queue is next, in its order"
        );
        let mut mixed: Vec<i64> = after[3..].iter().map(|r| r.0).collect();
        mixed.sort_unstable();
        assert_eq!(mixed, vec![4, 5, 9, 10, 11, 12], "{mode:?}");
        assert!(
            after[3..].iter().all(|r| r.1 == "playlist" && !r.2),
            "{mode:?}: every row keeps its source and mark"
        );

        apply_shuffle_mode(&pool, ShuffleMode::Off).await.unwrap();
        assert_eq!(rows(&pool).await, before, "{mode:?}: unshuffle restores");
        assert_eq!(super::current_index(&pool).await, 2, "{mode:?}");
    }
}

/// A queue edited between a shuffle's read and its write still comes
/// out dense: an entry gone since is skipped, and the cursor follows
/// the playing entry rather than an index.
#[tokio::test]
async fn unshuffle_skips_a_track_removed_since() {
    let pool = pool_with_tracks().await;
    fill_queue(
        &pool,
        "album",
        Some(1),
        &[1, 2, 3, 4],
        1,
        false,
        UserQueue::Drop,
    )
    .await
    .unwrap();
    apply_shuffle_mode(&pool, ShuffleMode::Tracks)
        .await
        .unwrap();
    // Removing a folder cascades to the queue without touching the
    // pre-shuffle snapshot, which still names track 3.
    sqlx::query("DELETE FROM track WHERE id = 3")
        .execute(&pool)
        .await
        .unwrap();

    apply_shuffle_mode(&pool, ShuffleMode::Off).await.unwrap();

    assert_eq!(track_order(&pool).await, vec![1, 2, 4]);
    let positions: Vec<i64> =
        sqlx::query_scalar("SELECT position FROM queue_item ORDER BY position")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(positions, vec![0, 1, 2]);
    assert_eq!(current_track(&pool).await.unwrap().unwrap().id, 2);
}
