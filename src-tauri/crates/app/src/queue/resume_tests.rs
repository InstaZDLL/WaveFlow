//! What Play starts on a player with nothing loaded (#1109).

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

/// Tracks 1 to 3, four minutes each, one artist, no album; track 3 is no
/// longer available.
async fn library(pool: &SqlitePool) {
    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
         INSERT INTO artist (id, name, canonical_name) VALUES (1, 'a', 'a');",
    )
    .execute(pool)
    .await
    .unwrap();
    for id in 1..=3_i64 {
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, primary_artist, duration_ms,
                                added_at, is_available)
             VALUES (?, 1, ?, ?, 1, 0, 't', 1, 240000, 0, ?)",
        )
        .bind(id)
        .bind(format!("/l/{id}.mp3"))
        .bind(format!("h{id}"))
        .bind(i64::from(id != 3))
        .execute(pool)
        .await
        .unwrap();
    }
}

/// The resume point the ten-second ticker left: `track_id` at `position_ms`.
async fn stored(pool: &SqlitePool, track_id: i64, position_ms: u64) {
    persist_resume_point(pool, track_id, position_ms)
        .await
        .unwrap();
}

async fn target(pool: &SqlitePool, finished: Option<i64>) -> Option<(i64, u64)> {
    resume_target(pool, finished)
        .await
        .unwrap()
        .map(|(track, position_ms)| (track.id, position_ms))
}

/// The reported case: the last write landed a few seconds before the end,
/// outside the two seconds `resume_position` starts over. The track ran out
/// since, and Play starts it from the top instead of replaying that stretch.
#[tokio::test]
async fn a_finished_track_starts_over_whatever_the_stored_point_says() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    stored(&pool, 1, 234_000).await;
    assert_eq!(target(&pool, Some(1)).await, Some((1, 0)));
}

/// A track shorter than the ticker's ten seconds may end the queue without
/// a write of its own: the stored point still names the track before it.
#[tokio::test]
async fn the_finished_track_wins_over_another_stored_track() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    stored(&pool, 1, 60_000).await;
    assert_eq!(target(&pool, Some(2)).await, Some((2, 0)));
}

/// Nothing finished — a launch, a stop mid-track: the stored point is
/// resumed where it is, as before.
#[tokio::test]
async fn without_a_finished_track_the_stored_point_is_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    stored(&pool, 1, 234_000).await;
    assert_eq!(target(&pool, None).await, Some((1, 234_000)));
    // The last two seconds still start over (#898).
    stored(&pool, 1, 239_000).await;
    assert_eq!(target(&pool, None).await, Some((1, 0)));
}

/// A finished track that has gone from the library since is not loaded:
/// the stored point is used like any other time.
#[tokio::test]
async fn a_finished_track_no_longer_available_falls_back_to_the_stored_point() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    stored(&pool, 1, 60_000).await;
    assert_eq!(target(&pool, Some(3)).await, Some((1, 60_000)));
}
