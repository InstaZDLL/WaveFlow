use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// The real migrator against a real database with `foreign_keys` on, so
/// the `CHECK` on the digest and the cascade from `track` are the ones
/// that ship rather than a fixture's idea of them.
async fn pool() -> SqlitePool {
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
    pool
}

/// One library and one track pointing at `path`.
async fn seed(pool: &SqlitePool, path: &str, size: i64, modified: i64) {
    sqlx::raw_sql(
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, ?, 'h', ?, ?, 'T', 1000, 0, 1, 0, 0, 0, 0)",
    )
    .bind(path)
    .bind(size)
    .bind(modified)
    .execute(pool)
    .await
    .unwrap();
}

fn temp_file(contents: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "waveflow-hash-test-{}",
        blake3::hash(format!("{:?}", std::time::SystemTime::now()).as_bytes()).to_hex()
    ));
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn a_digest_is_computed_once_and_read_back() {
    let file = temp_file(b"some audio");
    let path = file.to_string_lossy().to_string();
    let pool = pool().await;
    seed(&pool, &path, 10, 42).await;

    let first = full_hash(&pool, 1, &path, 10, 42).await.unwrap().unwrap();
    assert_eq!(first.len(), 64);

    // Delete the file: a second call that still answers proves the answer
    // came from the cache and not from another read.
    std::fs::remove_file(&file).unwrap();
    let second = full_hash(&pool, 1, &path, 10, 42).await.unwrap().unwrap();
    assert_eq!(first, second);
}

#[tokio::test]
async fn a_changed_file_invalidates_its_digest() {
    let file = temp_file(b"before");
    let path = file.to_string_lossy().to_string();
    let pool = pool().await;
    seed(&pool, &path, 6, 42).await;
    let before = full_hash(&pool, 1, &path, 6, 42).await.unwrap().unwrap();

    // Same bytes on disk, but the row now says the file moved: the cache
    // must not answer for a size and mtime it was not computed against.
    std::fs::write(&file, b"after!").unwrap();
    let after = full_hash(&pool, 1, &path, 6, 43).await.unwrap().unwrap();
    assert_ne!(before, after);
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn an_unreadable_file_is_reported_rather_than_fatal() {
    let pool = pool().await;
    seed(&pool, "/nowhere/at/all.flac", 1, 1).await;
    assert!(full_hash(&pool, 1, "/nowhere/at/all.flac", 1, 1)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn forgetting_makes_the_next_call_read_the_file_again() {
    let file = temp_file(b"one");
    let path = file.to_string_lossy().to_string();
    let pool = pool().await;
    seed(&pool, &path, 3, 42).await;
    full_hash(&pool, 1, &path, 3, 42).await.unwrap().unwrap();
    forget(&pool, 1).await.unwrap();

    // The entry is gone, so the file is read — and it is not there.
    std::fs::remove_file(&file).unwrap();
    assert!(full_hash(&pool, 1, &path, 3, 42).await.unwrap().is_none());
}

#[tokio::test]
async fn a_batch_read_returns_only_valid_entries() {
    let file = temp_file(b"bytes");
    let path = file.to_string_lossy().to_string();
    let pool = pool().await;
    seed(&pool, &path, 5, 42).await;
    full_hash(&pool, 1, &path, 5, 42).await.unwrap().unwrap();

    assert_eq!(cached_digests(&pool, &[1]).await.unwrap().len(), 1);
    // A track the caller did not ask about is not returned even when cached.
    assert!(cached_digests(&pool, &[2]).await.unwrap().is_empty());
    assert!(cached_digests(&pool, &[]).await.unwrap().is_empty());

    // The row says the file moved: the entry is no longer valid, and the
    // batch read must not hand it back as if it described the new bytes.
    sqlx::raw_sql("UPDATE track SET file_modified = 43 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert!(cached_digests(&pool, &[1]).await.unwrap().is_empty());
    std::fs::remove_file(&file).ok();
}

/// The distinction the whole module exists for: discovery answers from the
/// cache, verification reads the file. A rewrite that preserves size and
/// mtime is the case that tells them apart — and the case a verification
/// exists to catch.
#[tokio::test]
async fn verification_reads_the_file_where_discovery_does_not() {
    let file = temp_file(b"before");
    let path = file.to_string_lossy().to_string();
    let pool = pool().await;
    seed(&pool, &path, 6, 42).await;
    let first = full_hash(&pool, 1, &path, 6, 42).await.unwrap().unwrap();

    // Same length, same recorded mtime, different bytes: the cache cannot
    // see this, and is not meant to.
    std::fs::write(&file, b"after!").unwrap();
    let discovered = full_hash(&pool, 1, &path, 6, 42).await.unwrap().unwrap();
    assert_eq!(discovered, first, "discovery must answer from the cache");

    let verified = verify(&pool, 1, &path, 6, 42).await.unwrap().unwrap();
    assert_ne!(verified, first, "verification must read the file");
    // And it repaired the entry on the way past.
    assert_eq!(
        cached_digests(&pool, &[1]).await.unwrap().get(&1),
        Some(&verified)
    );
    std::fs::remove_file(&file).ok();
}

#[tokio::test]
async fn verifying_a_missing_file_reports_rather_than_fails() {
    let pool = pool().await;
    seed(&pool, "/nowhere/at/all.flac", 1, 1).await;
    assert!(verify(&pool, 1, "/nowhere/at/all.flac", 1, 1)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_linked_track_is_not_a_candidate() {
    let pool = pool().await;
    seed(&pool, "/m/1.flac", 1, 1).await;
    assert_eq!(unlinked_tracks(&pool).await.unwrap().len(), 1);

    sqlx::raw_sql(
        "INSERT INTO remote_track_link
                 (local_track_id, remote_track_id, method, verified_full_hash,
                  status, playback_preference, confirmed_at, verified_at)
             VALUES (1, 'r-1', 'exact_full_hash',
                     '0000000000000000000000000000000000000000000000000000000000000000',
                     'confirmed', 'local_first', 0, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(unlinked_tracks(&pool).await.unwrap().is_empty());
}
