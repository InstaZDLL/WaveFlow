use super::{
    append_to_user_queue, current_track, fill_queue, insert_after_current, peek_next, peek_step,
    read_album_ordered, write_setting_i64, Direction, RepeatMode,
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

    fill_queue(&pool, "radio", None, &[1, 2], 0, true)
        .await
        .unwrap();
    assert!(read_album_ordered(&pool).await);

    fill_queue(&pool, "playlist", Some(7), &[1], 0, false)
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
        fill_queue(&pool, "radio", None, &[1, 2], 0, true)
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
    fill_queue(&pool, "radio", None, &[1, 2], 0, true)
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
    fill_queue(&pool, "playlist", Some(1), &[1, 2], 0, false)
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
