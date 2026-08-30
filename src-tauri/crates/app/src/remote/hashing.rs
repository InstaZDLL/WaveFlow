//! The whole-file digest of a local track, computed once and kept.
//!
//! `track.file_hash` is a partial digest by design — head and tail — so a
//! rescan does not read the whole library. The server's `full_hash` covers the
//! entire file. The two are incompatible by construction, which is what makes
//! local↔server identity expensive: it has to be established by reading files
//! in full.
//!
//! Reading the library once is the price of that identity and it is accepted
//! (server RFC-008 says so in as many words). Reading it once *per pass* is
//! not, which is what this module exists to prevent: an upload sweep, a
//! reconciliation scan and a second upload sweep an hour later would otherwise
//! each pay it in full.
//!
//! ## What invalidates an entry
//!
//! `(file_size, file_modified)` — the same pair the scanner's fast path trusts
//! to decide a file has not changed. A retag rewrites the file and moves at
//! least one of them, so the cached digest is dropped rather than handed to a
//! server as an identity it no longer describes. The narrow case the scanner
//! also has (a tool that rewrites a file preserving both) is not worse here
//! than there: the same deep rescan that fixes the scanner's view rewrites
//! `track.file_modified`, which invalidates this too.

use std::path::{Path, PathBuf};

use sqlx::{Row, SqlitePool};

use crate::error::{AppError, AppResult};

/// Retrieves a track's full digest, reusing a cached value when its file metadata
/// matches and computing a new value otherwise. Unreadable files produce `None`.
///
/// # Arguments
///
/// * `track_id` - Identifier of the local track.
/// * `path` - Path to the track file.
/// * `size` - File size associated with the requested digest.
/// * `modified` - File modification timestamp associated with the requested digest.
///
/// # Returns
///
/// The full digest, or `None` when the file cannot be read.
///
/// # Examples
///
/// ```no_run
/// # use sqlx::SqlitePool;
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let pool = SqlitePool::connect("sqlite::memory:").await?;
/// let digest = full_hash(&pool, 42, "/music/track.flac", 123_456, 1_700_000_000).await?;
/// # let _ = digest;
/// # Ok(())
/// # }
/// ```
pub async fn full_hash(
    pool: &SqlitePool,
    track_id: i64,
    path: &str,
    size: i64,
    modified: i64,
) -> AppResult<Option<String>> {
    let cached: Option<String> = sqlx::query_scalar(
        "SELECT full_hash FROM local_full_hash
          WHERE track_id = ? AND file_size = ? AND file_modified = ?",
    )
    .bind(track_id)
    .bind(size)
    .bind(modified)
    .fetch_optional(pool)
    .await?;
    if let Some(hash) = cached {
        return Ok(Some(hash));
    }

    let owned = PathBuf::from(path);
    let hashed = tokio::task::spawn_blocking(move || {
        waveflow_core::scanner::hash_file_full(Path::new(&owned))
    })
    .await
    .map_err(|err| AppError::Other(format!("hash task failed: {err}")))?;
    let Ok(hash) = hashed else {
        return Ok(None);
    };

    // Written with the size and mtime the caller was told, not with a fresh
    // stat: those are what the digest was computed against as far as anything
    // reading this row is concerned, and re-stating here would let a file that
    // changed mid-read be recorded as if it had not.
    sqlx::query(
        "INSERT INTO local_full_hash (track_id, full_hash, file_size, file_modified, computed_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
             full_hash = excluded.full_hash,
             file_size = excluded.file_size,
             file_modified = excluded.file_modified,
             computed_at = excluded.computed_at",
    )
    .bind(track_id)
    .bind(&hash)
    .bind(size)
    .bind(modified)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(pool)
    .await?;
    Ok(Some(hash))
}

/// Lists available local tracks that have no server-track link, ordered by track ID.
///
/// Each tuple contains the track ID, file path, file size, and modification time.
///
/// # Examples
///
/// ```no_run
/// # async fn example(pool: &sqlx::SqlitePool) {
/// let tracks = unlinked_tracks(pool).await.unwrap();
/// for (track_id, path, size, modified) in tracks {
///     println!("{track_id}: {path} ({size} bytes, modified {modified})");
/// }
/// # }
/// ```
///
/// # Errors
///
/// Returns an error if the database query or row conversion fails.
pub async fn unlinked_tracks(pool: &SqlitePool) -> AppResult<Vec<(i64, String, i64, i64)>> {
    let rows = sqlx::query(
        "SELECT t.id, t.file_path, t.file_size, t.file_modified
           FROM track t
          WHERE t.is_available = 1
            AND NOT EXISTS (SELECT 1 FROM remote_track_link l WHERE l.local_track_id = t.id)
          ORDER BY t.id",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("id")?,
                row.try_get("file_path")?,
                row.try_get("file_size")?,
                row.try_get("file_modified")?,
            ))
        })
        .collect()
}

/// Removes the cached full digest for a track.
///
/// # Examples
///
/// ```no_run
/// # async fn example(pool: &sqlx::SqlitePool) -> Result<(), Box<dyn std::error::Error>> {
/// forget(pool, 42).await?;
/// # Ok(())
/// # }
/// ```
pub async fn forget(pool: &SqlitePool, track_id: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM local_full_hash WHERE track_id = ?")
        .bind(track_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
