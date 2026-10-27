use super::*;
use sqlx::SqlitePool;

/// Mirrors the post-migration shape: `track_id` nullable with
/// `ON DELETE SET NULL`, plus the snapshot columns. Foreign keys are
/// enabled explicitly — SQLite defaults them OFF per connection, and
/// without them the orphaning behaviour under test never fires.
async fn fixture_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "PRAGMA foreign_keys = ON;
             CREATE TABLE artist (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL
             );
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 folder_id INTEGER,
                 file_path TEXT NOT NULL,
                 file_hash TEXT NOT NULL,
                 title TEXT NOT NULL,
                 primary_artist INTEGER REFERENCES artist(id),
                 is_available INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE play_event (
                 id INTEGER PRIMARY KEY,
                 track_id INTEGER REFERENCES track(id) ON DELETE SET NULL,
                 played_at INTEGER NOT NULL,
                 listened_ms INTEGER NOT NULL,
                 completed INTEGER NOT NULL DEFAULT 0,
                 skipped INTEGER NOT NULL DEFAULT 0,
                 source_type TEXT,
                 source_id INTEGER,
                 snapshot_hash TEXT,
                 snapshot_path TEXT,
                 snapshot_artist TEXT,
                 snapshot_title TEXT
             );",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn seed_track(pool: &SqlitePool, id: i64, path: &str, hash: &str, title: &str, artist: &str) {
    sqlx::query("INSERT OR IGNORE INTO artist (id, name) VALUES (1, ?)")
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track (id, folder_id, file_path, file_hash, title, primary_artist)
             VALUES (?, 1, ?, ?, ?, 1)",
    )
    .bind(id)
    .bind(path)
    .bind(hash)
    .bind(title)
    .execute(pool)
    .await
    .unwrap();
}

/// One play event, snapshotted the way `record_play_event` does.
async fn seed_orphan_event(
    pool: &SqlitePool,
    hash: Option<&str>,
    path: Option<&str>,
    artist: Option<&str>,
    title: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO play_event
                (track_id, played_at, listened_ms,
                 snapshot_hash, snapshot_path, snapshot_artist, snapshot_title)
             VALUES (NULL, 0, 1000, ?, ?, ?, ?)",
    )
    .bind(hash)
    .bind(path)
    .bind(artist)
    .bind(title)
    .execute(pool)
    .await
    .unwrap();
}

async fn attached_track_ids(pool: &SqlitePool) -> Vec<Option<i64>> {
    sqlx::query_scalar("SELECT track_id FROM play_event ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn event_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM play_event")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The regression at the heart of #367: deleting the tracks under a
/// folder used to cascade and destroy the history. It must now orphan
/// it instead — the rows survive, waiting to be re-attached.
#[tokio::test]
async fn deleting_a_track_orphans_its_history_instead_of_erasing_it() {
    let pool = fixture_pool().await;
    seed_track(&pool, 1, "/music/a.flac", "hash-a", "Song", "Artist").await;
    sqlx::query(
        "INSERT INTO play_event (track_id, played_at, listened_ms, snapshot_hash)
             VALUES (1, 0, 1000, 'hash-a')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM track WHERE folder_id = 1")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(event_count(&pool).await, 1, "the history must survive");
    assert_eq!(attached_track_ids(&pool).await, vec![None]);
}

/// Same bytes, re-added — the strongest match.
#[tokio::test]
async fn an_orphan_is_reattached_by_file_hash() {
    let pool = fixture_pool().await;
    seed_orphan_event(&pool, Some("hash-a"), Some("/old/a.flac"), None, None).await;
    seed_track(&pool, 7, "/new/place/a.flac", "hash-a", "Song", "Artist").await;

    let n = reattach_orphaned_play_events(&pool).await.unwrap();

    assert_eq!(n, 1);
    assert_eq!(attached_track_ids(&pool).await, vec![Some(7)]);
}

/// A tag edit rewrites the file through lofty, so the blake3 moves
/// while the path does not. The hash pass misses; the path pass
/// catches it.
#[tokio::test]
async fn an_orphan_is_reattached_by_path_when_the_hash_changed() {
    let pool = fixture_pool().await;
    seed_orphan_event(&pool, Some("old-hash"), Some("/music/a.flac"), None, None).await;
    seed_track(
        &pool,
        7,
        "/music/a.flac",
        "rewritten-hash",
        "Song",
        "Artist",
    )
    .await;

    let n = reattach_orphaned_play_events(&pool).await.unwrap();

    assert_eq!(n, 1);
    assert_eq!(attached_track_ids(&pool).await, vec![Some(7)]);
}

/// A re-rip: different bytes, different location. Only the metadata
/// still lines up.
#[tokio::test]
async fn an_orphan_is_reattached_by_artist_and_title_as_a_last_resort() {
    let pool = fixture_pool().await;
    seed_orphan_event(
        &pool,
        Some("old-hash"),
        Some("/old/a.flac"),
        Some("Artist"),
        Some("Song"),
    )
    .await;
    seed_track(&pool, 7, "/rerip/a.flac", "new-hash", "Song", "Artist").await;

    let n = reattach_orphaned_play_events(&pool).await.unwrap();

    assert_eq!(n, 1);
    assert_eq!(attached_track_ids(&pool).await, vec![Some(7)]);
}

/// Priority matters: when several keys could match different tracks,
/// the hash must win. Otherwise a weaker key could claim the event
/// first and pin the history to the wrong song.
#[tokio::test]
async fn the_hash_match_wins_over_the_looser_keys() {
    let pool = fixture_pool().await;
    seed_orphan_event(
        &pool,
        Some("hash-right"),
        Some("/decoy/path.flac"),
        Some("Artist"),
        Some("Song"),
    )
    .await;
    // Lower id, so a naive `ORDER BY t.id` without the priority
    // ordering would pick this one.
    seed_track(&pool, 2, "/decoy/path.flac", "hash-wrong", "Song", "Artist").await;
    seed_track(&pool, 9, "/right/song.flac", "hash-right", "Song", "Artist").await;

    let n = reattach_orphaned_play_events(&pool).await.unwrap();

    assert_eq!(n, 1);
    assert_eq!(attached_track_ids(&pool).await, vec![Some(9)]);
}

/// Nothing matches: the event stays orphaned rather than being
/// dropped or attached to something arbitrary. It can still be
/// rescued by a later scan.
#[tokio::test]
async fn an_unmatchable_orphan_is_left_alone() {
    let pool = fixture_pool().await;
    seed_orphan_event(
        &pool,
        Some("nowhere"),
        Some("/gone.flac"),
        Some("Ghost"),
        Some("Missing"),
    )
    .await;
    seed_track(&pool, 7, "/other.flac", "other-hash", "Other", "Other").await;

    let n = reattach_orphaned_play_events(&pool).await.unwrap();

    assert_eq!(n, 0);
    assert_eq!(event_count(&pool).await, 1, "the event must not be dropped");
    assert_eq!(attached_track_ids(&pool).await, vec![None]);
}

/// An unavailable track is a file that vanished. Attaching history to
/// it would just orphan the event again on the next pass.
#[tokio::test]
async fn an_unavailable_track_is_not_a_match() {
    let pool = fixture_pool().await;
    seed_orphan_event(&pool, Some("hash-a"), None, None, None).await;
    seed_track(&pool, 7, "/music/a.flac", "hash-a", "Song", "Artist").await;
    sqlx::query("UPDATE track SET is_available = 0 WHERE id = 7")
        .execute(&pool)
        .await
        .unwrap();

    let n = reattach_orphaned_play_events(&pool).await.unwrap();

    assert_eq!(n, 0);
    assert_eq!(attached_track_ids(&pool).await, vec![None]);
}

/// Already-attached events must not be touched — the pass is for
/// orphans only, and re-running it has to be free.
#[tokio::test]
async fn an_attached_event_is_never_rewritten() {
    let pool = fixture_pool().await;
    seed_track(&pool, 1, "/music/a.flac", "hash-a", "Song", "Artist").await;
    seed_track(&pool, 2, "/music/b.flac", "hash-a", "Song", "Artist").await;
    sqlx::query(
        "INSERT INTO play_event (track_id, played_at, listened_ms, snapshot_hash)
             VALUES (2, 0, 1000, 'hash-a')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let n = reattach_orphaned_play_events(&pool).await.unwrap();

    assert_eq!(n, 0, "no orphans, so nothing to do");
    assert_eq!(
        attached_track_ids(&pool).await,
        vec![Some(2)],
        "must not be re-pointed at the lower-id duplicate",
    );
}
