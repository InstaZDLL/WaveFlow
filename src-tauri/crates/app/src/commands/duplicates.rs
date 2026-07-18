//! Duplicate-track detection.
//!
//! Groups byte-identical files (in different folders, regardless of
//! metadata) so the user can prune copies. The scan-time `file_hash` is
//! a *partial* blake3 (size + head + tail, for speed) and isn't
//! distinguishable from a legacy full digest, so the command prefilters
//! candidates by byte SIZE — a format-stable field every duplicate
//! shares — then re-verifies each candidate with a full-content hash
//! before returning, since the UI lets the user delete from a group.
//! Identity is content-only, so renames / re-tags still group correctly
//! — but two re-encodes of the same source (e.g. CBR vs VBR rips) won't
//! match because the bytes differ. That's a fingerprinting problem and
//! out of scope for this MVP.

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DuplicateTrack {
    pub id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub file_path: String,
    pub file_size: i64,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub duration_ms: i64,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    pub file_hash: String,
    pub tracks: Vec<DuplicateTrack>,
}

/// Find every set of tracks that share a `file_hash` (i.e. are
/// byte-identical copies of the same file). Returns one entry per
/// duplicate group, each containing every track in the group ordered
/// by `added_at` ASC so the oldest copy renders first — usually the
/// one the user wants to keep.
#[tauri::command]
pub async fn find_duplicates(state: tauri::State<'_, AppState>) -> AppResult<Vec<DuplicateGroup>> {
    let pool = state.require_profile_pool().await?;

    // Pull every candidate row in one round-trip. Candidates are tracks
    // that share their byte SIZE with another — a format-stable
    // prefilter that catches duplicates whether their stored hash is a
    // legacy full digest or a newer partial one (both are 64-char blake3
    // hex, indistinguishable). The full-content verification below forms
    // the real groups.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        String,
        Option<String>,
        Option<String>,
        String,
        i64,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
    )> = sqlx::query_as(
        r#"
        SELECT t.id,
               t.title,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                   SELECT ar.name FROM track_artist ta
                   JOIN artist ar ON ar.id = ta.artist_id
                   WHERE ta.track_id = t.id
                   ORDER BY ta.position
               )) AS artist_name,
               al.title AS album_title,
               t.file_path,
               t.file_size,
               t.bitrate,
               t.sample_rate,
               t.duration_ms,
               t.added_at
          FROM track t
          LEFT JOIN album al ON al.id = t.album_id
         WHERE t.file_size IN (
             SELECT file_size FROM track
              WHERE is_available = 1
              GROUP BY file_size
              HAVING COUNT(*) > 1
         )
           AND t.is_available = 1
         ORDER BY t.file_size, t.added_at ASC
        "#,
    )
    .fetch_all(&*pool)
    .await?;

    let candidates: Vec<DuplicateTrack> = rows
        .into_iter()
        .map(
            |(
                id,
                title,
                artist_name,
                album_title,
                file_path,
                file_size,
                bitrate,
                sample_rate,
                duration_ms,
                added_at,
            )| DuplicateTrack {
                id,
                title,
                artist_name,
                album_title,
                file_path,
                file_size,
                bitrate,
                sample_rate,
                duration_ms,
                added_at,
            },
        )
        .collect();

    // The size prefilter is only a *candidate* set, and a delete follows,
    // so confirm byte-identity with a full-content hash — computed
    // off-thread and only on these few candidate files. This both forms
    // the real groups and closes the partial-hash middle-byte blind spot.
    let groups = tokio::task::spawn_blocking(move || verify_groups_full_content(candidates))
        .await
        .map_err(|e| AppError::Other(format!("dedup verify task failed: {e}")))?;
    Ok(groups)
}

/// Bucket the size-prefiltered candidates by a full-content hash so only
/// genuinely byte-identical files stay grouped — independent of whatever
/// (legacy full / new partial) digest is stored on each row. A file that
/// can't be read is dropped (we won't offer to delete what we can't
/// verify); buckets that collapse to a single track are not duplicates.
fn verify_groups_full_content(candidates: Vec<DuplicateTrack>) -> Vec<DuplicateGroup> {
    use std::collections::HashMap;

    let mut by_full: HashMap<String, Vec<DuplicateTrack>> = HashMap::new();
    for track in candidates {
        match waveflow_core::scanner::hash_file_full(std::path::Path::new(&track.file_path)) {
            Ok(full) => by_full.entry(full).or_default().push(track),
            Err(err) => tracing::warn!(
                path = %track.file_path,
                error = %err,
                "dedup full-content hash failed; excluding track"
            ),
        }
    }

    let mut out: Vec<DuplicateGroup> = by_full
        .into_iter()
        .filter(|(_, tracks)| tracks.len() > 1)
        .map(|(full_hash, mut tracks)| {
            // Oldest copy first — usually the one the user keeps.
            tracks.sort_by_key(|t| t.added_at);
            DuplicateGroup {
                file_hash: full_hash,
                tracks,
            }
        })
        .collect();
    // HashMap iteration order is non-deterministic; sort groups so the UI
    // renders the same order across calls.
    out.sort_by_key(|g| g.tracks.first().map(|t| t.added_at).unwrap_or(0));
    out
}

/// Remove a list of tracks from the database. The audio files on
/// disk are NOT touched — the user can delete those manually if they
/// want. Detaches every cascading row (track_artist, track_genre,
/// playlist_track, play_event, etc.) via the schema's ON DELETE
/// CASCADE constraints. Returns the count of rows actually deleted
/// so the UI can render an honest toast.
#[tauri::command]
pub async fn delete_tracks(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<i64>,
) -> AppResult<i64> {
    use tauri::Emitter;
    if track_ids.is_empty() {
        return Ok(0);
    }
    let pool = state.require_profile_pool().await?;

    let mut deleted = 0i64;
    let mut tx = pool.begin().await?;
    for id in track_ids {
        // Phase 4.d.0.3: capture (library_id, file_path) BEFORE
        // the DELETE so we can enqueue the matching sync op while
        // the row still exists. Skipped silently when the row is
        // already gone (a concurrent scan / second delete-tracks
        // call could race us).
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT library_id, file_path FROM track WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| AppError::Other(format!("lookup track {id}: {e}")))?;
        let res = sqlx::query("DELETE FROM track WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Other(format!("delete track {id}: {e}")))?;
        deleted += res.rows_affected() as i64;
        if let Some((library_id, file_path)) = row {
            // User-initiated delete (NOT a library / folder cascade
            // — those replay on the server when the parent library
            // op lands). The sync op stays in the same tx as the
            // DELETE so the two either both land or both roll back.
            crate::sync::track_emit::emit_track_delete_in_tx(&mut tx, library_id, &file_path)
                .await?;
        }
    }
    tx.commit().await?;

    // Phase 4.d.0.3: nudge the drain so the per-track delete ops
    // ship before the drain's idle poll wakes up. Matches the
    // convention every other sync-emitting command follows.
    state.drain.notify();

    let _ = app.emit("library:rescanned", ());
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::{verify_groups_full_content, DuplicateTrack};

    fn track(path: String, added_at: i64) -> DuplicateTrack {
        DuplicateTrack {
            id: added_at,
            title: "t".into(),
            artist_name: None,
            album_title: None,
            file_path: path,
            file_size: 0,
            bitrate: None,
            sample_rate: None,
            duration_ms: 0,
            added_at,
        }
    }

    #[test]
    fn groups_identical_files_by_content_not_stored_hash() {
        // Two byte-identical files + one different. In a mixed library
        // the identical pair could carry a legacy full hash on one row
        // and a new partial hash on the other; verify reads the bytes,
        // so they still group. `DuplicateTrack` carries no stored hash —
        // that's the point: identity is content-derived here.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.bin");
        std::fs::write(&a, b"same bytes").unwrap();
        std::fs::write(&b, b"same bytes").unwrap();
        std::fs::write(&c, b"different bytes").unwrap();

        let groups = verify_groups_full_content(vec![
            track(a.to_string_lossy().into_owned(), 1),
            track(b.to_string_lossy().into_owned(), 2),
            track(c.to_string_lossy().into_owned(), 3),
        ]);

        assert_eq!(groups.len(), 1, "only the identical pair forms a group");
        let mut ids: Vec<i64> = groups[0].tracks.iter().map(|t| t.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn unreadable_files_are_excluded() {
        let groups = verify_groups_full_content(vec![
            track("/no/such/file/x.bin".into(), 1),
            track("/no/such/file/y.bin".into(), 2),
        ]);
        assert!(groups.is_empty());
    }
}
