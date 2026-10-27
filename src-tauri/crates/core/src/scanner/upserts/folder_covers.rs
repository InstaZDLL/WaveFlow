//! Folder covers: re-link each album to the sidecar cover its folder
//! holds, when the album's artwork is sidecar-sourced or absent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::error::{CoreError, CoreResult};
use crate::scanner::extract::{extract_folder_cover, ArtworkSource};

use super::upsert_artwork;

/// One album due for a cover refresh, as observed before the filesystem
/// walk. `observed_artwork_id` / `observed_hash` are what the album
/// pointed at when the candidate list was built, and are re-checked at
/// write time — see [`link_folder_cover_if_eligible`].
struct CoverCandidate {
    album_id: i64,
    dir: PathBuf,
    observed_artwork_id: Option<i64>,
    observed_hash: Option<String>,
}

/// Point an album at `artwork_id`, but only if it still holds the artwork
/// the caller saw and that artwork is still sidecar-sourced (or absent).
/// Returns whether the row changed.
///
/// [`refresh_folder_covers`] already filters on the source rule when it
/// builds its candidate list, but that read happens *before* the blocking
/// directory walk, which can run for seconds on a large library — long
/// enough for the album's cover to change underneath. Re-checking as part
/// of the write closes that window: the conditions and the update are one
/// statement, so nothing can slip between them.
///
/// Two distinct hazards, hence two conditions:
///
/// - `expected_artwork_id` makes this a compare-and-swap, so a *newer*
///   sidecar resolved by a concurrent scan of the same album isn't rolled
///   back to the one this pass computed from a staler read. Bound with
///   `IS` rather than `=` because the observed value is legitimately NULL
///   for an album that had no artwork, and SQLite's `=` never matches
///   NULL.
/// - The source allowlist stops a cover the user uploaded (or an
///   enrichment fetched) mid-walk from being overwritten by a sidecar.
///
/// The boolean comes from `rows_affected`, i.e. what the database
/// actually changed rather than what the caller intended, so an album
/// that moved on cannot inflate the scan summary.
async fn link_folder_cover_if_eligible(
    conn: &mut sqlx::SqliteConnection,
    album_id: i64,
    expected_artwork_id: Option<i64>,
    artwork_id: i64,
) -> CoreResult<bool> {
    let result = sqlx::query(
        "UPDATE album
            SET artwork_id = ?, artwork_source = 'folder'
          WHERE id = ?
            AND artwork_id IS ?
            AND (artwork_id IS NULL OR artwork_source = 'folder')",
    )
    .bind(artwork_id)
    .bind(album_id)
    .bind(expected_artwork_id)
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Reconcile sidecar cover art against the albums that live next to it.
///
/// The scanner's fast path keys on the **audio** files' `(mtime, size)`,
/// so replacing `cover.jpg` in a folder changes nothing it looks at: the
/// tracks are skipped, `extract_folder_cover` never runs, and the album
/// keeps its old picture forever (issue #366, symptom A). The file that
/// changed simply isn't one the scanner watches.
///
/// This pass closes that gap by working per **directory** rather than
/// per file — one `read_dir` + one hash for a whole album, instead of
/// one per track — and updating any album whose sidecar no longer
/// matches what's stored. No extra bookkeeping is needed: `artwork.hash`
/// is already the blake3 of the picture bytes, so the stored row is its
/// own baseline for comparison.
///
/// Three deliberate restrictions:
///
/// - **Only sidecar-sourced artwork is replaced.** A row whose `source`
///   is anything other than `folder` was put there by something that
///   outranks a sidecar: `embedded` (extraction treats the sidecar as a
///   *fallback* for tracks whose tag carries no picture), `user` (a
///   manual upload), `deezer` (an enrichment fetch). The guard is an
///   allowlist — `folder` or no artwork at all — so a source added later
///   is preserved by default rather than silently clobbered. Caveat:
///   `artwork` rows are deduped on hash alone, so `source` records who
///   inserted the bytes first, not where this album got them. An image
///   that is embedded in one album and a sidecar next to another is
///   labelled `embedded` for both, and the sidecar one then stops being
///   refreshable here. See issue #401.
/// - **A deleted sidecar does not blank the album.** A vanished cover is
///   far more likely to be a transient state (files being reorganised)
///   than a request for a blank album.
/// - **A multi-directory album resolves against its first directory**
///   in sorted order, evaluated over *all* of the album's tracks rather
///   than only those under the scanned folder. Restricting it to the
///   scanned folder would make the winning directory depend on which
///   folder was scanned, i.e. flip the picture back and forth — exactly
///   the non-determinism this rule exists to prevent.
pub async fn refresh_folder_covers(
    pool: &SqlitePool,
    artwork_dir: &Path,
    folder_id: i64,
) -> CoreResult<u32> {
    // The subquery scopes the work to albums this scan could have
    // touched; the outer query then takes *every* available track of
    // those albums, so directory selection sees the album's full extent
    // no matter which folder triggered the scan.
    let rows: Vec<(i64, String, Option<i64>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT t.album_id, t.file_path, al.artwork_id, aw.hash, al.artwork_source
           FROM track t
           JOIN album al ON al.id = t.album_id
           LEFT JOIN artwork aw ON aw.id = al.artwork_id
          WHERE t.is_available = 1
            AND t.album_id IN (
                SELECT DISTINCT album_id
                  FROM track
                 WHERE folder_id = ?
                   AND album_id IS NOT NULL
                   AND is_available = 1
            )",
    )
    .bind(folder_id)
    .fetch_all(pool)
    .await?;

    // album_id → (winning directory, artwork id, hash, source) as
    // observed now. The directory is kept as a running minimum instead of
    // collecting every path and sorting at the end — same answer, no
    // allocation proportional to track count.
    let mut albums: HashMap<i64, (PathBuf, Option<i64>, Option<String>, Option<String>)> =
        HashMap::new();
    for (album_id, file_path, artwork_id, hash, source) in rows {
        let Some(dir) = Path::new(&file_path).parent().map(Path::to_path_buf) else {
            continue;
        };
        albums
            .entry(album_id)
            .and_modify(|entry| {
                if dir < entry.0 {
                    entry.0 = dir.clone();
                }
            })
            .or_insert((dir, artwork_id, hash, source));
    }

    // Drop the albums this pass must not touch before doing any
    // filesystem work, so an untouchable album never costs a read.
    let candidates: Vec<CoverCandidate> = albums
        .into_iter()
        .filter(|(_, (_, _, _, source))| matches!(source.as_deref(), None | Some("folder")))
        .map(
            |(album_id, (dir, observed_artwork_id, observed_hash, _))| CoverCandidate {
                album_id,
                dir,
                observed_artwork_id,
                observed_hash,
            },
        )
        .collect();
    if candidates.is_empty() {
        return Ok(0);
    }

    // Resolve every distinct directory in one blocking batch. This is
    // `read_dir` + a full read + a blake3 per directory — hundreds of
    // megabytes on a large library — and must not run on the async
    // runtime, which would stall unrelated tasks (IPC replies, the
    // progress emitter) for seconds at the tail of every scan.
    //
    // Several albums commonly share one directory (a compilation split
    // across album rows, or singles dumped together), hence the dedup:
    // that's the read + hash we're trying not to repeat.
    let mut wanted: Vec<PathBuf> = candidates.iter().map(|c| c.dir.clone()).collect();
    wanted.sort();
    wanted.dedup();

    let artwork_dir = artwork_dir.to_path_buf();
    let covers: HashMap<PathBuf, Option<(String, String)>> =
        tokio::task::spawn_blocking(move || {
            wanted
                .into_iter()
                .map(|dir| {
                    // `extract_folder_cover` takes a file path and looks
                    // at its parent, so hand it a synthetic child.
                    let found = extract_folder_cover(&dir.join("_"), &artwork_dir)
                        .map(|cover| (cover.hash, cover.format));
                    (dir, found)
                })
                .collect()
        })
        .await
        .map_err(|e| CoreError::Other(format!("folder cover resolution join: {e}")))?;

    // One transaction for the whole batch — the scanner is the single
    // writer and a per-album autocommit would serialise N round-trips
    // through WAL for no benefit. Also makes each artwork insert and the
    // album row that points at it land together.
    let mut tx = pool.begin().await?;
    let mut updated: u32 = 0;

    for candidate in candidates {
        let Some((hash, format)) = covers.get(&candidate.dir).cloned().flatten() else {
            continue;
        };
        if candidate.observed_hash.as_deref() == Some(hash.as_str()) {
            continue;
        }

        let album_id = candidate.album_id;
        let artwork_id = upsert_artwork(&mut tx, &hash, &format, ArtworkSource::Folder).await?;
        if !link_folder_cover_if_eligible(
            &mut tx,
            album_id,
            candidate.observed_artwork_id,
            artwork_id,
        )
        .await?
        {
            tracing::debug!(album_id, "album cover changed under us; leaving it alone");
            continue;
        }
        updated += 1;
        tracing::debug!(album_id, %hash, "refreshed album cover from folder sidecar");
    }

    tx.commit().await?;
    Ok(updated)
}

#[cfg(test)]
mod tests;
