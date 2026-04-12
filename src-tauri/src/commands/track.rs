use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{error::AppResult, state::AppState};

/// Track row returned to the frontend, already joined with album + primary
/// artist so the UI never has to issue a follow-up query per row. Ordering
/// follows the "Artist → Album → Disc → Track number" convention used by
/// most native music players.
///
/// `artwork_path` is resolved in Rust (not SQL) because the artwork file
/// lives under the per-profile data dir, which the database itself doesn't
/// know about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub album_title: Option<String>,
    pub artist_name: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub file_path: String,
    pub file_size: i64,
    pub added_at: i64,
    pub artwork_path: Option<String>,
}

/// Raw row shape as it comes out of the SQL query — kept private because the
/// public `Track` struct adds a derived `artwork_path` that the database
/// doesn't know how to compute.
#[derive(FromRow)]
struct TrackRow {
    id: i64,
    library_id: i64,
    title: String,
    album_title: Option<String>,
    artist_name: Option<String>,
    duration_ms: i64,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    year: Option<i64>,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    channels: Option<i64>,
    file_path: String,
    file_size: i64,
    added_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

/// List tracks. When `library_id` is `Some`, only tracks from that library
/// are returned. When `None`, tracks across **all** libraries are shown —
/// the "Ma musique" mode where the concept of multiple libraries is hidden
/// from the user.
#[tauri::command]
pub async fn list_tracks(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
) -> AppResult<Vec<Track>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let rows = sqlx::query_as::<_, TrackRow>(
        r#"
        SELECT t.id, t.library_id, t.title,
               al.title AS album_title,
               ar.name  AS artist_name,
               t.duration_ms, t.track_number, t.disc_number, t.year,
               t.bitrate, t.sample_rate, t.channels,
               t.file_path, t.file_size, t.added_at,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format
          FROM track t
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         ORDER BY ar.canonical_name COLLATE NOCASE,
                  al.canonical_title COLLATE NOCASE,
                  t.disc_number,
                  t.track_number,
                  t.title COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .fetch_all(&pool)
    .await?;

    let tracks = rows
        .into_iter()
        .map(|row| {
            let artwork_path = match (row.artwork_hash, row.artwork_format) {
                (Some(hash), Some(format)) => Some(
                    artwork_dir
                        .join(format!("{}.{}", hash, format))
                        .to_string_lossy()
                        .to_string(),
                ),
                _ => None,
            };
            Track {
                id: row.id,
                library_id: row.library_id,
                title: row.title,
                album_title: row.album_title,
                artist_name: row.artist_name,
                duration_ms: row.duration_ms,
                track_number: row.track_number,
                disc_number: row.disc_number,
                year: row.year,
                bitrate: row.bitrate,
                sample_rate: row.sample_rate,
                channels: row.channels,
                file_path: row.file_path,
                file_size: row.file_size,
                added_at: row.added_at,
                artwork_path,
            }
        })
        .collect();

    Ok(tracks)
}

/// Full-text search via the `track_fts` FTS5 virtual table (kept in sync
/// by triggers). Returns up to 50 matching tracks, ranked by relevance.
/// The query is sanitized: double-quotes are stripped and a trailing `*`
/// is appended for prefix matching so "moon" finds "Moonlight".
#[tauri::command]
pub async fn search_tracks(
    state: tauri::State<'_, AppState>,
    query: String,
) -> AppResult<Vec<Track>> {
    let trimmed = query.trim().replace('"', "");
    if trimmed.is_empty() {
        return Ok(vec![]);
    }
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // Build FTS5 query: split words and add * for prefix matching.
    let fts_query = trimmed
        .split_whitespace()
        .map(|w| format!("{w}*"))
        .collect::<Vec<_>>()
        .join(" ");

    let rows = sqlx::query_as::<_, TrackRow>(
        r#"
        SELECT t.id, t.library_id, t.title,
               al.title AS album_title,
               ar.name  AS artist_name,
               t.duration_ms, t.track_number, t.disc_number, t.year,
               t.bitrate, t.sample_rate, t.channels,
               t.file_path, t.file_size, t.added_at,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format
          FROM track_fts fts
          JOIN track t        ON t.id  = fts.rowid
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE track_fts MATCH ? AND t.is_available = 1
         ORDER BY rank
         LIMIT 50
        "#,
    )
    .bind(&fts_query)
    .fetch_all(&pool)
    .await?;

    let tracks = rows
        .into_iter()
        .map(|row| {
            let artwork_path = match (row.artwork_hash, row.artwork_format) {
                (Some(hash), Some(format)) => Some(
                    artwork_dir
                        .join(format!("{}.{}", hash, format))
                        .to_string_lossy()
                        .to_string(),
                ),
                _ => None,
            };
            Track {
                id: row.id,
                library_id: row.library_id,
                title: row.title,
                album_title: row.album_title,
                artist_name: row.artist_name,
                duration_ms: row.duration_ms,
                track_number: row.track_number,
                disc_number: row.disc_number,
                year: row.year,
                bitrate: row.bitrate,
                sample_rate: row.sample_rate,
                channels: row.channels,
                file_path: row.file_path,
                file_size: row.file_size,
                added_at: row.added_at,
                artwork_path,
            }
        })
        .collect();

    Ok(tracks)
}

/// Toggle the liked state of a track. If already liked → unlike (DELETE),
/// if not → like (INSERT). Returns `true` if the track is now liked.
#[tauri::command]
pub async fn toggle_like_track(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<bool> {
    let pool = state.require_profile_pool().await?;

    let exists: Option<i64> =
        sqlx::query_scalar("SELECT track_id FROM liked_track WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(&pool)
            .await?;

    if exists.is_some() {
        sqlx::query("DELETE FROM liked_track WHERE track_id = ?")
            .bind(track_id)
            .execute(&pool)
            .await?;
        Ok(false)
    } else {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query("INSERT INTO liked_track (track_id, liked_at) VALUES (?, ?)")
            .bind(track_id)
            .bind(now)
            .execute(&pool)
            .await?;
        Ok(true)
    }
}

/// Return the set of liked track IDs so the frontend can render hearts
/// without N+1 queries. Cheap because `liked_track` is indexed.
#[tauri::command]
pub async fn list_liked_track_ids(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    let ids = sqlx::query_scalar("SELECT track_id FROM liked_track ORDER BY liked_at DESC")
        .fetch_all(&pool)
        .await?;
    Ok(ids)
}

/// List every liked track with full metadata, ordered by most recently
/// liked first. Used by the LikedView.
#[tauri::command]
pub async fn list_liked_tracks(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<Track>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let rows = sqlx::query_as::<_, TrackRow>(
        r#"
        SELECT t.id, t.library_id, t.title,
               al.title AS album_title,
               ar.name  AS artist_name,
               t.duration_ms, t.track_number, t.disc_number, t.year,
               t.bitrate, t.sample_rate, t.channels,
               t.file_path, t.file_size, t.added_at,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format
          FROM liked_track lt
          JOIN track t        ON t.id  = lt.track_id
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE t.is_available = 1
         ORDER BY lt.liked_at DESC
        "#,
    )
    .fetch_all(&pool)
    .await?;

    let tracks = rows
        .into_iter()
        .map(|row| {
            let artwork_path = match (row.artwork_hash, row.artwork_format) {
                (Some(hash), Some(format)) => Some(
                    artwork_dir
                        .join(format!("{}.{}", hash, format))
                        .to_string_lossy()
                        .to_string(),
                ),
                _ => None,
            };
            Track {
                id: row.id,
                library_id: row.library_id,
                title: row.title,
                album_title: row.album_title,
                artist_name: row.artist_name,
                duration_ms: row.duration_ms,
                track_number: row.track_number,
                disc_number: row.disc_number,
                year: row.year,
                bitrate: row.bitrate,
                sample_rate: row.sample_rate,
                channels: row.channels,
                file_path: row.file_path,
                file_size: row.file_size,
                added_at: row.added_at,
                artwork_path,
            }
        })
        .collect();

    Ok(tracks)
}
