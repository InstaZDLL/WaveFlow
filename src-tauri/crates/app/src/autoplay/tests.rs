use super::*;
use crate::queue::{self, Direction, RepeatMode};

/// The repo's own migrations, both databases, with `app` attached the
/// way every profile pool has it and `foreign_keys` on.
async fn pool(dir: &std::path::Path) -> SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let opts_for = |path: &std::path::Path| {
        SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true)
    };
    let app_path = dir.join("app.db");
    let app_pool = SqlitePool::connect_with(opts_for(&app_path)).await.unwrap();
    sqlx::migrate!("../../migrations/app")
        .run(&app_pool)
        .await
        .unwrap();
    app_pool.close().await;
    let attach = format!(
        "ATTACH DATABASE '{}' AS app",
        app_path.display().to_string().replace('\'', "''")
    );
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(move |conn, _meta| {
            let attach = attach.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(attach))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(opts_for(&dir.join("profile.db")))
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

/// Artists 1 (the seed's), 2 (listed as similar) and 3 (unrelated):
/// tracks 1-5 by artist 1, 6-10 by artist 2, 11-30 by artist 3.
async fn library(pool: &SqlitePool) {
    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO artist (id, name, canonical_name)
                  VALUES (1, 'Seed', 'seed'), (2, 'Near', 'near'), (3, 'Far', 'far');
             INSERT INTO app.lastfm_similar (name_canonical, payload, source, fetched_at, expires_at)
                  VALUES ('seed', '[{\"name\":\"Near\"}]', 'lastfm', 0, 0);",
    )
    .execute(pool)
    .await
    .unwrap();
    for id in 1..=30_i64 {
        let artist = match id {
            1..=5 => 1,
            6..=10 => 2,
            _ => 3,
        };
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, primary_artist, duration_ms, added_at)
                 VALUES (?, 1, ?, ?, 1, 0, 't', ?, 1000, 0)",
        )
        .bind(id)
        .bind(format!("/l/{id}.flac"))
        .bind(format!("h{id}"))
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn enable(pool: &SqlitePool, on: bool) {
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES (?, ?, 'bool', 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(SETTING_KEY)
    .bind(if on { "true" } else { "false" })
    .execute(pool)
    .await
    .unwrap();
}

async fn queued(pool: &SqlitePool) -> Vec<i64> {
    sqlx::query_scalar("SELECT track_id FROM queue_item ORDER BY position")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A queue of `ids` with the cursor on its last entry.
async fn at_end(pool: &SqlitePool, ids: &[i64]) {
    queue::fill_queue(pool, "album", None, ids, ids.len() - 1, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn off_by_default_and_the_queue_still_runs_out() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    at_end(&pool, &[1, 2]).await;
    assert!(!is_enabled(&pool).await);
    assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
    assert_eq!(queued(&pool).await, vec![1, 2]);
    assert!(queue::peek_step(&pool, Direction::Next, RepeatMode::Off)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn an_exhausted_queue_is_topped_up_and_the_next_step_lands_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enable(&pool, true).await;
    at_end(&pool, &[1, 2]).await;
    assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_some());
    let ids = queued(&pool).await;
    assert_eq!(ids.len(), 2 + BATCH);
    let added = &ids[2..];
    let mut unique = added.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), BATCH, "a track added twice: {added:?}");
    assert!(!added.contains(&1) && !added.contains(&2), "{added:?}");
    let (index, track) = queue::peek_step(&pool, Direction::Next, RepeatMode::Off)
        .await
        .unwrap()
        .expect("the step past the old end lands on an added track");
    assert_eq!(index, 2);
    assert_eq!(track.id, added[0]);
}

#[tokio::test]
async fn related_artists_come_first_and_the_seed_artist_is_capped() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    // Seed = track 1 (artist 1). Artist 1 has 4 more, artist 2 five.
    at_end(&pool, &[1]).await;
    let picked = pick(&pool, 1, BATCH, 0).await.unwrap();
    let seed_artist = picked.iter().filter(|id| (2..=5).contains(*id)).count();
    let similar = picked.iter().filter(|id| (6..=10).contains(*id)).count();
    assert_eq!(seed_artist, SEED_ARTIST_CAP, "{picked:?}");
    assert_eq!(
        similar, 5,
        "every similar track before the rest: {picked:?}"
    );
    assert_eq!(picked.len(), BATCH);
    assert!(!picked.contains(&1), "{picked:?}");
}

#[tokio::test]
async fn nothing_queued_or_just_played_is_picked_while_something_else_is_left() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    at_end(&pool, &[1, 6, 7]).await;
    // Tracks 8-10 were played a minute ago; 2 four hours ago.
    let now = 10 * 60 * 60 * 1000_i64;
    for (track, at) in [
        (8, now - 60_000),
        (9, now - 60_000),
        (10, now - 60_000),
        (2, now - 4 * 60 * 60 * 1000),
    ] {
        sqlx::query(
            "INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (?, ?, 1000)",
        )
        .bind(track)
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
    }
    // A play whose track was deleted since: its row stays, with no
    // track. It must not empty the "not played lately" tiers.
    sqlx::query("INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (NULL, ?, 1000)")
        .bind(now - 60_000)
        .execute(&pool)
        .await
        .unwrap();
    let picked = pick(&pool, 7, 40, now).await.unwrap();
    for id in [1, 6, 7] {
        assert!(!picked.contains(&id), "{id} is queued: {picked:?}");
    }
    // 27 tracks are not queued; 3 of them were just played and come
    // last, once everything else is in.
    assert_eq!(picked.len(), 27, "{picked:?}");
    let tail: Vec<i64> = picked[24..].to_vec();
    for id in [8, 9, 10] {
        assert!(tail.contains(&id), "{id} should come last: {picked:?}");
    }
    assert!(picked[..24].contains(&2), "played hours ago is fair game");
}

#[tokio::test]
async fn only_the_end_of_a_queue_with_repeat_off_is_topped_up() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enable(&pool, true).await;
    // Repeat all / one already have a next entry.
    at_end(&pool, &[1, 2]).await;
    assert!(top_up(&pool, RepeatMode::All).await.unwrap().is_none());
    assert!(top_up(&pool, RepeatMode::One).await.unwrap().is_none());
    // A cursor before the end has a next entry too.
    queue::fill_queue(&pool, "album", None, &[1, 2, 3], 0, false)
        .await
        .unwrap();
    assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
    assert_eq!(queued(&pool).await, vec![1, 2, 3]);
    // An empty queue has nothing to continue from.
    queue::clear(&pool).await.unwrap();
    assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
    assert!(queued(&pool).await.is_empty());
}

#[tokio::test]
async fn switched_off_it_adds_nothing_more() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enable(&pool, true).await;
    at_end(&pool, &[1]).await;
    assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_some());
    let len = queued(&pool).await.len() as i64;
    queue::commit_index(&pool, len - 1).await.unwrap();
    enable(&pool, false).await;
    assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
    assert_eq!(queued(&pool).await.len() as i64, len);
}

#[tokio::test]
async fn a_batch_taken_back_leaves_the_queue_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enable(&pool, true).await;
    at_end(&pool, &[1, 2]).await;
    let batch = top_up(&pool, RepeatMode::Off).await.unwrap().unwrap();
    assert!(batch.covers(2) && batch.covers(1 + BATCH as i64) && !batch.covers(1));
    batch.undo(&pool).await;
    assert_eq!(queued(&pool).await, vec![1, 2]);
    assert_eq!(queue::current_index(&pool).await, 1);
}

#[tokio::test]
async fn the_end_of_a_session_reaching_a_pick_is_told_so() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enable(&pool, true).await;
    at_end(&pool, &[1, 2]).await;
    top_up(&pool, RepeatMode::Off).await.unwrap().unwrap();
    assert!(reaches_picks(&pool, 2, "album").await);
    assert!(!reaches_picks(&pool, 1, "album").await, "an album row");
    assert!(
        !reaches_picks(&pool, 2, "radio").await,
        "a radio carries on"
    );
}

#[tokio::test]
async fn the_seed_artist_goes_past_its_cap_only_when_nothing_else_is_left() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    // Everything but artist 1 is queued: its four other tracks are all
    // that is left, and all of them are taken.
    let others: Vec<i64> = (1..=1).chain(6..=30).collect();
    at_end(&pool, &others).await;
    let mut picked = pick(&pool, 1, BATCH, 0).await.unwrap();
    picked.sort_unstable();
    assert_eq!(picked, vec![2, 3, 4, 5]);
}

#[tokio::test]
async fn more_of_the_seed_artist_comes_before_a_track_just_heard() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    // Artist 2 and half of artist 3 are queued; the other half of
    // artist 3 (21-30) was played a minute ago. Only the seed artist
    // has tracks left that were not heard lately, one past its cap.
    let queued_ids: Vec<i64> = (1..=1).chain(6..=20).collect();
    at_end(&pool, &queued_ids).await;
    let now = 10 * 60 * 60 * 1000_i64;
    for track in 21..=30_i64 {
        sqlx::query(
            "INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (?, ?, 1000)",
        )
        .bind(track)
        .bind(now - 60_000)
        .execute(&pool)
        .await
        .unwrap();
    }
    let picked = pick(&pool, 1, BATCH, now).await.unwrap();
    for id in 2..=5 {
        assert!(
            picked.contains(&id),
            "{id} was not heard lately: {picked:?}"
        );
    }
    assert_eq!(picked.len(), BATCH);
}

#[tokio::test]
async fn a_library_with_nothing_left_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    enable(&pool, true).await;
    let all: Vec<i64> = (1..=30).collect();
    at_end(&pool, &all).await;
    assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
    assert_eq!(queued(&pool).await.len(), 30);
}

async fn set_key(pool: &SqlitePool, ids: std::ops::RangeInclusive<i64>, key: &str) {
    for id in ids {
        sqlx::query("UPDATE track SET musical_key = ? WHERE id = ?")
            .bind(key)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

async fn automix(pool: &SqlitePool, on: bool) {
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.automix', ?, 'bool', 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(if on { "true" } else { "false" })
    .execute(pool)
    .await
    .unwrap();
}

/// With AutoMix on, a track whose key sits beside the seed's comes first
/// within its tier, and only within it: the seed artist and the similar
/// ones are taken exactly as without it.
#[tokio::test]
async fn with_automix_on_a_compatible_key_breaks_the_tie_within_a_tier() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    // Fourteen tracks of the unrelated artist, so a draw of KEY_DRAW per
    // pick sees all of them and the outcome does not hang on chance.
    sqlx::query("DELETE FROM track WHERE id > 24")
        .execute(&pool)
        .await
        .unwrap();
    set_key(&pool, 1..=1, "C").await;
    // A fifth up, the relative minor (Camelot notation), a fifth down.
    set_key(&pool, 20..=20, "G").await;
    set_key(&pool, 21..=21, "8A").await;
    set_key(&pool, 22..=22, "F").await;
    // Far from C on the wheel, or no key at all.
    set_key(&pool, 11..=19, "F#").await;
    automix(&pool, true).await;
    at_end(&pool, &[1]).await;

    let picked = pick(&pool, 1, BATCH, 0).await.unwrap();
    assert_eq!(picked.len(), BATCH);
    // The tiers before the open library are as they were.
    assert_eq!(
        picked.iter().filter(|id| (2..=5).contains(*id)).count(),
        SEED_ARTIST_CAP
    );
    assert_eq!(picked.iter().filter(|id| (6..=10).contains(*id)).count(), 5);
    // The two picks left come from the open library, both compatible.
    let rest: Vec<i64> = picked.iter().copied().filter(|id| *id > 10).collect();
    assert_eq!(rest.len(), 2, "{picked:?}");
    assert!(rest.iter().all(|id| (20..=22).contains(id)), "{picked:?}");
}

/// Off, the key plays no part: across many draws, incompatible tracks are
/// picked from the open library as often as chance has it.
#[tokio::test]
async fn with_automix_off_the_key_plays_no_part() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    library(&pool).await;
    sqlx::query("DELETE FROM track WHERE id > 24")
        .execute(&pool)
        .await
        .unwrap();
    set_key(&pool, 1..=1, "C").await;
    set_key(&pool, 20..=22, "G").await;
    set_key(&pool, 11..=19, "F#").await;
    automix(&pool, false).await;
    at_end(&pool, &[1]).await;

    let mut incompatible = 0;
    for _ in 0..20 {
        let picked = pick(&pool, 1, BATCH, 0).await.unwrap();
        incompatible += picked.iter().filter(|id| (11..=19).contains(*id)).count();
    }
    assert!(incompatible > 0, "the key should not have steered the draw");
}
