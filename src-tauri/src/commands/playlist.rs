//! Playlist CRUD commands.
//!
//! Mirrors [`super::library`] but targets the `playlist` / `playlist_track`
//! tables. A playlist is an ordered, user-curated collection of tracks that
//! can cross library boundaries — the track rows themselves still live under
//! a `library_id`, the playlist just points at them through `playlist_track`.
//!
//! All mutations bump `playlist.updated_at` so the sidebar (which orders
//! playlists by `updated_at DESC` as a tie-break) reflects recent edits.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{
    commands::track::Track,
    error::{AppError, AppResult},
    state::AppState,
};

/// Playlist row returned to the frontend, with a denormalized `track_count`
/// and `total_duration_ms` so the sidebar row can display
/// "Playlist · N titres" without issuing a follow-up query per playlist.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub color_id: String,
    pub icon_id: String,
    pub is_smart: i64,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub track_count: i64,
    pub total_duration_ms: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreatePlaylistInput {
    pub name: String,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

/// Partial update payload — any field left as `None` is preserved via
/// SQL `COALESCE`. Same shape as [`super::library::UpdateLibraryInput`].
#[derive(Debug, Deserialize)]
pub struct UpdatePlaylistInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

/// Raw row shape for the joined track query — private because the public
/// [`Track`] struct holds a derived `artwork_path` the DB can't compute.
#[derive(FromRow)]
struct PlaylistTrackRow {
    id: i64,
    library_id: i64,
    title: String,
    album_id: Option<i64>,
    album_title: Option<String>,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    duration_ms: i64,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    year: Option<i64>,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    channels: Option<i64>,
    bit_depth: Option<i64>,
    codec: Option<String>,
    file_path: String,
    file_size: i64,
    added_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    rating: Option<i64>,
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// List every playlist in the active profile, ordered by `position` first
/// (for future manual reordering) then `updated_at DESC` as a tie-break so
/// recently-edited playlists float to the top by default.
#[tauri::command]
pub async fn list_playlists(state: tauri::State<'_, AppState>) -> AppResult<Vec<Playlist>> {
    let pool = state.require_profile_pool().await?;

    let playlists = sqlx::query_as::<_, Playlist>(
        r#"
        SELECT p.id, p.name, p.description, p.color_id, p.icon_id,
               p.is_smart, p.position, p.created_at, p.updated_at,
               COALESCE(pc.track_count,       0) AS track_count,
               COALESCE(pc.total_duration_ms, 0) AS total_duration_ms
          FROM playlist p
          LEFT JOIN (
              SELECT pt.playlist_id,
                     COUNT(*)                AS track_count,
                     SUM(t.duration_ms)      AS total_duration_ms
                FROM playlist_track pt
                JOIN track t ON t.id = pt.track_id
               WHERE t.is_available = 1
               GROUP BY pt.playlist_id
          ) pc ON pc.playlist_id = p.id
         ORDER BY p.position ASC, p.updated_at DESC
        "#,
    )
    .fetch_all(&pool)
    .await?;

    Ok(playlists)
}

/// Fetch a single playlist by id. Used by the PlaylistView header.
#[tauri::command]
pub async fn get_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<Playlist> {
    let pool = state.require_profile_pool().await?;

    let playlist = sqlx::query_as::<_, Playlist>(
        r#"
        SELECT p.id, p.name, p.description, p.color_id, p.icon_id,
               p.is_smart, p.position, p.created_at, p.updated_at,
               COALESCE(pc.track_count,       0) AS track_count,
               COALESCE(pc.total_duration_ms, 0) AS total_duration_ms
          FROM playlist p
          LEFT JOIN (
              SELECT pt.playlist_id,
                     COUNT(*)                AS track_count,
                     SUM(t.duration_ms)      AS total_duration_ms
                FROM playlist_track pt
                JOIN track t ON t.id = pt.track_id
               WHERE t.is_available = 1
               GROUP BY pt.playlist_id
          ) pc ON pc.playlist_id = p.id
         WHERE p.id = ?
        "#,
    )
    .bind(playlist_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| {
        AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        ))
    })?;

    Ok(playlist)
}

/// Create a new playlist. Follows the same defaults as
/// [`CreatePlaylistModal`](../../../../src/components/common/CreatePlaylistModal.tsx):
/// violet color, music icon.
#[tauri::command]
pub async fn create_playlist(
    state: tauri::State<'_, AppState>,
    input: CreatePlaylistInput,
) -> AppResult<Playlist> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("playlist name cannot be empty".into()));
    }
    let color_id = input.color_id.unwrap_or_else(|| "violet".to_string());
    let icon_id = input.icon_id.unwrap_or_else(|| "music".to_string());
    let now = now_millis();

    let pool = state.require_profile_pool().await?;

    let insert = sqlx::query(
        "INSERT INTO playlist
             (name, description, color_id, icon_id, is_smart, position,
              created_at, updated_at)
         VALUES (?, ?, ?, ?, 0, 0, ?, ?)",
    )
    .bind(&name)
    .bind(input.description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await?;

    let id = insert.last_insert_rowid();

    Ok(Playlist {
        id,
        name,
        description: input.description,
        color_id,
        icon_id,
        is_smart: 0,
        position: 0,
        created_at: now,
        updated_at: now,
        track_count: 0,
        total_duration_ms: 0,
    })
}

/// Partial update — name/description/color/icon. Bumps `updated_at`.
#[tauri::command]
pub async fn update_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    input: UpdatePlaylistInput,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;

    // Precise error for missing id instead of a silent "0 rows updated".
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM playlist WHERE id = ?")
        .bind(playlist_id)
        .fetch_optional(&pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        )));
    }

    let trimmed_name = input.name.as_ref().map(|s| s.trim().to_string());
    if let Some(name) = &trimmed_name {
        if name.is_empty() {
            return Err(AppError::Other("playlist name cannot be empty".into()));
        }
    }

    let now = now_millis();
    sqlx::query(
        "UPDATE playlist
            SET name        = COALESCE(?, name),
                description = COALESCE(?, description),
                color_id    = COALESCE(?, color_id),
                icon_id     = COALESCE(?, icon_id),
                updated_at  = ?
          WHERE id = ?",
    )
    .bind(trimmed_name.as_deref())
    .bind(input.description.as_deref())
    .bind(input.color_id.as_deref())
    .bind(input.icon_id.as_deref())
    .bind(now)
    .bind(playlist_id)
    .execute(&pool)
    .await?;

    Ok(())
}

/// Delete a playlist. `ON DELETE CASCADE` on `playlist_track` removes the
/// track links, but the underlying `track` rows are preserved — they still
/// belong to their library.
#[tauri::command]
pub async fn delete_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;

    let result = sqlx::query("DELETE FROM playlist WHERE id = ?")
        .bind(playlist_id)
        .execute(&pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        )));
    }

    tracing::info!(playlist_id, "playlist deleted");
    Ok(())
}

/// List every track of a playlist in its stored order. Mirrors the SELECT in
/// [`super::track::list_tracks`] with an extra `JOIN playlist_track` so the
/// ordering follows the user's arrangement (`pt.position ASC`) instead of
/// the alphabetical artist/album/disc/track sort.
#[tauri::command]
pub async fn list_playlist_tracks(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<Vec<Track>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let rows = sqlx::query_as::<_, PlaylistTrackRow>(
        r#"
        SELECT t.id, t.library_id, t.title,
               t.album_id,
               al.title AS album_title,
               t.primary_artist AS artist_id,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               (SELECT GROUP_CONCAT(id, ',') FROM (
                  SELECT ta2.artist_id AS id FROM track_artist ta2
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_ids,
               t.duration_ms, t.track_number, t.disc_number, t.year,
               t.bitrate, t.sample_rate, t.channels,
               t.bit_depth, t.codec,
               t.file_path, t.file_size, t.added_at,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format,
               t.rating  AS rating
          FROM playlist_track pt
          JOIN track   t  ON t.id  = pt.track_id
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE pt.playlist_id = ? AND t.is_available = 1
         ORDER BY pt.position ASC
        "#,
    )
    .bind(playlist_id)
    .fetch_all(&pool)
    .await?;

    let tracks = rows
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(format)) => {
                        let full = artwork_dir
                            .join(format!("{}.{}", hash, format))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) =
                            crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            Track {
                id: row.id,
                library_id: row.library_id,
                title: row.title,
                album_id: row.album_id,
                album_title: row.album_title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                duration_ms: row.duration_ms,
                track_number: row.track_number,
                disc_number: row.disc_number,
                year: row.year,
                bitrate: row.bitrate,
                sample_rate: row.sample_rate,
                channels: row.channels,
                bit_depth: row.bit_depth,
                codec: row.codec,
                file_path: row.file_path,
                file_size: row.file_size,
                added_at: row.added_at,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
                rating: row.rating,
            }
        })
        .collect();

    Ok(tracks)
}

/// Append a single track to the end of a playlist. Idempotent — if the track
/// is already in the playlist the existing row is preserved and `updated_at`
/// is still bumped so the UI reflects the user's intent.
#[tauri::command]
pub async fn add_track_to_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let now = now_millis();

    // Compute next position in a single query so concurrent inserts from
    // different callers don't collide. Sqlite serializes writes at the
    // connection level, which is enough here.
    sqlx::query(
        "INSERT OR IGNORE INTO playlist_track (playlist_id, track_id, position, added_at)
         VALUES (?, ?,
                 (SELECT COALESCE(MAX(position), -1) + 1
                    FROM playlist_track
                   WHERE playlist_id = ?),
                 ?)",
    )
    .bind(playlist_id)
    .bind(track_id)
    .bind(playlist_id)
    .bind(now)
    .execute(&pool)
    .await?;

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(playlist_id)
        .execute(&pool)
        .await?;

    Ok(())
}

/// Bulk variant of [`add_track_to_playlist`]. Inserts every track one by one
/// (so positions stay contiguous even if some are duplicates) and returns
/// the count that were actually inserted.
#[tauri::command]
pub async fn add_tracks_to_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_ids: Vec<i64>,
) -> AppResult<u32> {
    if track_ids.is_empty() {
        return Ok(0);
    }

    let pool = state.require_profile_pool().await?;
    let now = now_millis();

    // Start from the current max + 1 and increment locally. A single
    // transaction keeps the write cheap and the positions contiguous.
    let mut tx = pool.begin().await?;
    let current_max: Option<i64> =
        sqlx::query_scalar("SELECT MAX(position) FROM playlist_track WHERE playlist_id = ?")
            .bind(playlist_id)
            .fetch_one(&mut *tx)
            .await?;
    let mut next_position = current_max.map(|p| p + 1).unwrap_or(0);
    let mut inserted: u32 = 0;

    for track_id in track_ids {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO playlist_track (playlist_id, track_id, position, added_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(next_position)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() > 0 {
            inserted += 1;
            next_position += 1;
        }
    }

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(inserted)
}

/// Remove a single track and renumber the tail so positions stay contiguous.
#[tauri::command]
pub async fn remove_track_from_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;

    let removed_position: Option<i64> = sqlx::query_scalar(
        "SELECT position FROM playlist_track WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist_id)
    .bind(track_id)
    .fetch_optional(&pool)
    .await?;

    let Some(pos) = removed_position else {
        // Not in the playlist — nothing to do.
        return Ok(());
    };

    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM playlist_track WHERE playlist_id = ? AND track_id = ?")
        .bind(playlist_id)
        .bind(track_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE playlist_track
            SET position = position - 1
          WHERE playlist_id = ? AND position > ?",
    )
    .bind(playlist_id)
    .bind(pos)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_millis())
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Move a track to a new absolute position inside a playlist, shifting
/// the surrounding rows so positions stay dense. Used by the
/// drag-and-drop UI. `new_position` is clamped to `[0, length - 1]`
/// so an out-of-range drop snaps to the nearest end instead of erroring.
///
/// `playlist_track.position` is non-UNIQUE (just an index for ORDER BY)
/// so the shift is a single bulk UPDATE per direction; no offset
/// gymnastics needed unlike the queue's UNIQUE-positioned variant.
#[tauri::command]
pub async fn reorder_playlist_track(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_id: i64,
    new_position: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let mut tx = pool.begin().await?;

    let from: Option<i64> = sqlx::query_scalar(
        "SELECT position FROM playlist_track WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist_id)
    .bind(track_id)
    .fetch_optional(&mut *tx)
    .await?;
    let from = from.ok_or_else(|| {
        AppError::Other(format!(
            "track {track_id} not in playlist {playlist_id}"
        ))
    })?;

    let len: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playlist_track WHERE playlist_id = ?",
    )
    .bind(playlist_id)
    .fetch_one(&mut *tx)
    .await?;
    let to = new_position.clamp(0, (len - 1).max(0));

    if from == to {
        tx.commit().await?;
        return Ok(());
    }

    if to > from {
        // Items in (from, to] shift down by 1.
        sqlx::query(
            "UPDATE playlist_track
                SET position = position - 1
              WHERE playlist_id = ? AND position > ? AND position <= ?",
        )
        .bind(playlist_id)
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;
    } else {
        // Items in [to, from) shift up by 1.
        sqlx::query(
            "UPDATE playlist_track
                SET position = position + 1
              WHERE playlist_id = ? AND position >= ? AND position < ?",
        )
        .bind(playlist_id)
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE playlist_track SET position = ?
          WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(to)
    .bind(playlist_id)
    .bind(track_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_millis())
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Add every available track matching a source (folder, album, artist) to a
/// playlist in one server-side transaction — avoids round-tripping thousands
/// of track IDs through the IPC bridge.
///
/// `source_type` must be one of `"folder"`, `"album"`, `"artist"`.
/// Returns the number of tracks actually inserted (duplicates are skipped).
#[tauri::command]
pub async fn add_source_to_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    source_type: String,
    source_id: i64,
) -> AppResult<u32> {
    let pool = state.require_profile_pool().await?;

    // Resolve the set of track IDs belonging to the source.
    let track_ids: Vec<i64> = match source_type.as_str() {
        "folder" => {
            sqlx::query_scalar(
                "SELECT id FROM track WHERE folder_id = ? AND is_available = 1
                 ORDER BY disc_number, track_number, title COLLATE NOCASE",
            )
            .bind(source_id)
            .fetch_all(&pool)
            .await?
        }
        "album" => {
            sqlx::query_scalar(
                "SELECT id FROM track WHERE album_id = ? AND is_available = 1
                 ORDER BY disc_number, track_number, title COLLATE NOCASE",
            )
            .bind(source_id)
            .fetch_all(&pool)
            .await?
        }
        "artist" => {
            sqlx::query_scalar(
                "SELECT id FROM track WHERE primary_artist = ? AND is_available = 1
                 ORDER BY title COLLATE NOCASE",
            )
            .bind(source_id)
            .fetch_all(&pool)
            .await?
        }
        other => {
            return Err(AppError::Other(format!(
                "unknown source_type '{other}', expected folder/album/artist"
            )));
        }
    };

    if track_ids.is_empty() {
        return Ok(0);
    }

    let now = now_millis();
    let mut tx = pool.begin().await?;

    let current_max: Option<i64> =
        sqlx::query_scalar("SELECT MAX(position) FROM playlist_track WHERE playlist_id = ?")
            .bind(playlist_id)
            .fetch_one(&mut *tx)
            .await?;
    let mut next_position = current_max.map(|p| p + 1).unwrap_or(0);
    let mut inserted: u32 = 0;

    for track_id in track_ids {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO playlist_track (playlist_id, track_id, position, added_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(next_position)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() > 0 {
            inserted += 1;
            next_position += 1;
        }
    }

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(inserted)
}
