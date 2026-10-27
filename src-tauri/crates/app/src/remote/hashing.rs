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
mod tests;
