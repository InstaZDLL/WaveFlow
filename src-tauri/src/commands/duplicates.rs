//! Duplicate-track detection.
//!
//! Groups tracks by their `file_hash` (blake3 of the audio bytes,
//! computed at scan time) so visually identical files in different
//! folders fall into the same group regardless of metadata. The hash
//! is content-only, so renames / re-tags after the initial scan
//! still group correctly — but two re-encodes of the same source
//! (e.g. CBR vs VBR rips of the same track) won't match because the
//! bytes differ. That's a fingerprinting problem and out of scope
//! for this MVP.

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

    // Pull every duplicated row in one round-trip — joining on the
    // hash subquery is cheaper than running N+1 queries.
    let rows: Vec<(
        String,
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
        SELECT t.file_hash,
               t.id,
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
         WHERE t.file_hash IN (
             SELECT file_hash FROM track
              WHERE is_available = 1
              GROUP BY file_hash
              HAVING COUNT(*) > 1
         )
           AND t.is_available = 1
         ORDER BY t.file_hash, t.added_at ASC
        "#,
    )
    .fetch_all(&pool)
    .await?;

    // Bucket by hash. The ORDER BY above guarantees consecutive rows
    // with the same hash, so a one-pass group is enough — no HashMap
    // needed.
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    for (
        hash,
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
    ) in rows
    {
        let track = DuplicateTrack {
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
        };
        match groups.last_mut() {
            Some(g) if g.file_hash == hash => g.tracks.push(track),
            _ => groups.push(DuplicateGroup {
                file_hash: hash,
                tracks: vec![track],
            }),
        }
    }
    Ok(groups)
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
        let res = sqlx::query("DELETE FROM track WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Other(format!("delete track {id}: {e}")))?;
        deleted += res.rows_affected() as i64;
    }
    tx.commit().await?;

    let _ = app.emit("library:rescanned", ());
    Ok(deleted)
}
