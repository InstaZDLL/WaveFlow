use std::sync::Mutex;

use super::*;

/// The repo's profile migrations, with `foreign_keys` on as on every
/// profile pool.
async fn pool(dir: &std::path::Path) -> SqlitePool {
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;
    let opts =
        SqliteConnectOptions::from_str(&format!("sqlite://{}", dir.join("profile.db").display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await.unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

/// Tracks 1 to 6, one artist, no album.
async fn library(pool: &SqlitePool) {
    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
         INSERT INTO artist (id, name, canonical_name) VALUES (1, 'a', 'a');",
    )
    .execute(pool)
    .await
    .unwrap();
    for id in 1..=6_i64 {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, primary_artist, duration_ms, added_at)
             VALUES (?, 1, ?, ?, 1, 0, 't', 1, 1000, 0)",
        )
        .bind(id)
        .bind(format!("/l/{id}.flac"))
        .bind(format!("h{id}"))
        .execute(pool)
        .await
        .unwrap();
    }
}

/// A queue of `ids`, the cursor on `at`, that track playing, and the
/// decoder having asked for the next one.
async fn playing(pool: &SqlitePool, ids: &[i64], at: usize) -> SharedPlayback {
    queue::fill_queue(pool, "album", None, ids, at, false)
        .await
        .unwrap();
    let shared = SharedPlayback::new();
    shared.current_track_id.store(ids[at], Ordering::Release);
    shared.next_wanted.store(true, Ordering::Release);
    shared
}

/// What a revision sends, as `(after, next)`: `next` is `None` for a
/// `ClearNextTrack`.
async fn revision(pool: &SqlitePool, shared: &SharedPlayback) -> Vec<(i64, Option<i64>)> {
    let sent = Mutex::new(Vec::new());
    revise_with(pool, shared, |cmd| {
        let entry = match cmd {
            AudioCmd::SetNextTrack {
                after, track_id, ..
            } => (after, Some(track_id)),
            AudioCmd::ClearNextTrack { after } => (after, None),
            other => panic!("unexpected {other:?}"),
        };
        sent.lock().unwrap().push(entry);
    })
    .await
    .unwrap();
    sent.into_inner().unwrap()
}

/// The reported case: "Play next" after the next track was prepared. The
/// decoder is sent the track the queue now names.
#[tokio::test]
async fn play_next_after_the_prefetch_sends_the_new_next_track() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2, 3], 0).await;
    queue::insert_after_current(&pool, &[5], "manual", None)
        .await
        .unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![(1, Some(5))]);
}

#[tokio::test]
async fn a_reorder_that_moves_another_track_up_sends_it() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2, 3, 4], 0).await;
    queue::reorder(&pool, 3, 1).await.unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![(1, Some(4))]);
}

/// Removing what came next leaves the queue without one: the prepared
/// track is cleared rather than played.
#[tokio::test]
async fn a_queue_with_no_next_track_any_more_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2], 0).await;
    queue::remove_range(&pool, 1, 2).await.unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![(1, None)]);
}

/// An MPD client clearing the queue leaves nothing to hand over.
#[tokio::test]
async fn a_cleared_queue_clears_the_next_track() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2, 3], 0).await;
    queue::clear(&pool).await.unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![(1, None)]);
}

/// Repeat decides what follows the last track: switching it on gives the
/// last track a next one, switching it off takes it away.
#[tokio::test]
async fn a_repeat_change_on_the_last_track_follows_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2], 1).await;
    queue::write_repeat_mode(&pool, RepeatMode::All)
        .await
        .unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![(2, Some(1))]);
    queue::write_repeat_mode(&pool, RepeatMode::Off)
        .await
        .unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![(2, None)]);
}

/// Before the decoder asks, a queue change sends nothing: the next track
/// is prepared at the usual moment, from the queue as it is then.
#[tokio::test]
async fn nothing_is_sent_before_the_decoder_asks() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2, 3], 0).await;
    shared.next_wanted.store(false, Ordering::Release);
    queue::insert_after_current(&pool, &[5], "manual", None)
        .await
        .unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![]);
}

/// A step has moved the cursor and its load is on its way: the answer
/// would be for a track about to be replaced, so none is sent.
#[tokio::test]
async fn nothing_is_sent_while_the_cursor_is_on_another_track() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2, 3], 0).await;
    queue::commit_index(&pool, 2).await.unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![]);
}

/// A queue edit waits for the publish lock: whoever holds it reads the
/// queue and acts on what it read before the edit lands. `hold` stands
/// for that holder; it returns once the edit is waiting.
async fn edit_waits<T: Send + 'static>(
    edit: impl std::future::Future<Output = T> + Send + 'static,
    hold: impl std::future::Future<Output = ()>,
) -> T {
    let publish = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let held = publish.clone().lock_owned().await;
    let edit = tokio::spawn({
        let publish = publish.clone();
        async move { edit_queue(publish.lock_owned(), edit).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    hold.await;
    drop(held);
    edit.await.unwrap()
}

/// An autoplay top-up has read the queue's end and is drawing its picks
/// when an MPD client clears the queue: the clear waits, then empties
/// the picks too, instead of being followed by them.
#[tokio::test]
async fn a_clear_waits_for_a_top_up_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    queue::fill_queue(&pool, "album", None, &[1, 2, 3], 2, false)
        .await
        .unwrap();
    let clear = {
        let pool = pool.clone();
        async move { queue::clear(&pool).await }
    };
    edit_waits(clear, async {
        assert_eq!(queue::queue_length(&pool).await.unwrap(), 3);
        queue::append(&pool, &[4], "radio", None).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(queue::queue_length(&pool).await.unwrap(), 0);
}

/// The prefetch has read the queue's next track and is sending it when
/// the queue is reordered: the reorder waits, so the track sent is the
/// one the queue named, and the revision that follows sends the new one.
#[tokio::test]
async fn a_reorder_waits_for_the_next_track_being_sent() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    let shared = playing(&pool, &[1, 2, 3, 4], 0).await;
    let reorder = {
        let pool = pool.clone();
        async move { queue::reorder(&pool, 3, 1).await }
    };
    edit_waits(reorder, async {
        assert_eq!(revision(&pool, &shared).await, vec![(1, Some(2))]);
    })
    .await
    .unwrap();
    assert_eq!(revision(&pool, &shared).await, vec![(1, Some(4))]);
}
