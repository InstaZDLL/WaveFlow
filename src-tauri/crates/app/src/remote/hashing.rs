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
//! ## Knowing is not verifying
//!
//! Two kinds of caller ask for a whole-file digest and they must not share a
//! path:
//!
//! - **Discovery** wants to *know* what a file hashes to — a reconciliation
//!   sweep, an upload survey, an import checking whether the library already
//!   holds some bytes. Thousands of files, and reading one twice buys nothing.
//!   These go through [`full_hash`], which answers from the cache.
//! - **Verification** wants to *check the bytes right now*, immediately before
//!   acting on them: confirming a link, converting a playlist. The point is
//!   precisely not to trust an earlier reading, so the cache would defeat it —
//!   an entry stays valid while `(file_size, file_modified)` match, and the one
//!   case that pair cannot see is a rewrite that preserved both. Those callers
//!   read the file, and [`verify`] is the way to do it while keeping the cache
//!   honest.
//!
//! Getting this backwards is silent in both directions: discovery through
//! `verify` reads the library twice, and verification through `full_hash`
//! confirms a link against bytes that may no longer be there.
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

/// The digest of one track, from the cache when it is still valid and from the
/// file otherwise.
///
/// Returns `None` when the file cannot be read — a track whose file has gone
/// is a fact the caller reports, not an error that should abort a sweep over
/// thousands of them.
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
    .await
    // Same reasoning as `remember_quietly`: the answer is already computed and
    // the cache is an optimisation, so a failed write must not lose it.
    .inspect_err(|err| {
        tracing::warn!(track_id, ?err, "could not cache a full-file digest");
    })
    .ok();
    Ok(Some(hash))
}

/// The cached digests of a set of tracks, in one query.
///
/// For the sweeps: asking per track would be one round trip per file across a
/// whole library, and the point of the cache is to avoid exactly that shape of
/// cost. Only entries still matching their `track` row's size and mtime come
/// back, so a caller can treat a miss as "not known" without checking anything
/// else.
pub async fn cached_digests(
    pool: &SqlitePool,
    track_ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, String>> {
    if track_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    // The candidate set is bounded by the library, and SQLite's variable limit
    // is not, so the join carries the validity condition rather than an `IN`
    // list: every valid entry is fetched and the caller keeps what it asked for.
    let rows = sqlx::query(
        "SELECT h.track_id, h.full_hash
           FROM local_full_hash h
           JOIN track t ON t.id = h.track_id
          WHERE t.file_size = h.file_size AND t.file_modified = h.file_modified",
    )
    .fetch_all(pool)
    .await?;
    let wanted: std::collections::HashSet<i64> = track_ids.iter().copied().collect();
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let id: i64 = row.try_get("track_id")?;
        if wanted.contains(&id) {
            out.insert(id, row.try_get("full_hash")?);
        }
    }
    Ok(out)
}

/// Persist a batch of freshly computed digests.
///
/// One transaction, because this runs after a sweep that may have hashed
/// thousands of files and SQLite has a single writer: a row at a time would
/// hold the write lock open across the whole batch for no gain.
///
/// Takes the pool rather than a caller's `&mut SqliteConnection`, unlike the
/// scanner's upsert helpers: those run *inside* a transaction their caller
/// already owns, while every caller here is between transactions and would
/// have to open one just to hand it over. Contention is left to sqlx's
/// five-second `busy_timeout`, and a collision that outlasts it is not retried
/// — see [`remember_quietly`] for why losing this batch is survivable in a way
/// that losing a batch of computed loudness is not.
pub async fn remember(pool: &SqlitePool, entries: &[(i64, String, i64, i64)]) -> AppResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let mut tx = pool.begin().await?;
    for (track_id, hash, size, modified) in entries {
        sqlx::query(
            "INSERT INTO local_full_hash
                 (track_id, full_hash, file_size, file_modified, computed_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(track_id) DO UPDATE SET
                 full_hash = excluded.full_hash,
                 file_size = excluded.file_size,
                 file_modified = excluded.file_modified,
                 computed_at = excluded.computed_at",
        )
        .bind(track_id)
        .bind(hash)
        .bind(size)
        .bind(modified)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// [`remember`], for the callers whose real work is already done.
///
/// A digest cache that could not be written is a slower next sweep, not a
/// failure: everything in the batch is recomputable by reading the files
/// again, which is exactly what the caller just did. Propagating the error
/// would abort a reconciliation — or discard a verification's answer — over a
/// write whose only purpose was to make the *next* run cheaper. That is also
/// why there is no retry loop here, unlike the analysis flush: that one
/// retries because a lost batch means recomputing an eight-second decode per
/// track and it has no other copy, while these digests are one file read away
/// and the sweep will simply pay it again.
pub async fn remember_quietly(pool: &SqlitePool, entries: &[(i64, String, i64, i64)]) {
    if let Err(err) = remember(pool, entries).await {
        tracing::warn!(
            rows = entries.len(),
            ?err,
            "could not cache full-file digests; the next sweep will re-read them"
        );
    }
}

/// Read the file **now** and record what it says.
///
/// For the callers that must not trust an earlier reading — confirming a link,
/// converting a playlist — where the whole point is that the bytes are checked
/// immediately before something irreversible happens. It refreshes the cache on
/// the way past, so a verification also repairs a stale entry.
pub async fn verify(
    pool: &SqlitePool,
    track_id: i64,
    path: &str,
    size: i64,
    modified: i64,
) -> AppResult<Option<String>> {
    let owned = PathBuf::from(path);
    let hashed = tokio::task::spawn_blocking(move || {
        waveflow_core::scanner::hash_file_full(Path::new(&owned))
    })
    .await
    .map_err(|err| AppError::Other(format!("hash task failed: {err}")))?;
    let Ok(hash) = hashed else {
        return Ok(None);
    };
    remember_quietly(pool, &[(track_id, hash.clone(), size, modified)]).await;
    Ok(Some(hash))
}

/// Every available local track that is not already linked to a server track,
/// oldest first so a resumed sweep makes the same progress twice.
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

/// Drop the cached digest of one track, for the paths that know the file
/// changed without waiting for a size or mtime comparison to notice.
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
}
