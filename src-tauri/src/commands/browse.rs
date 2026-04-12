//! Aggregate "browse" queries for a library: albums, artists, genres, folders.
//!
//! These commands back the Albums / Artistes / Genres / Dossiers tabs in the
//! library view. They all take a `library_id` and filter content to rows that
//! have at least one available track in that library — important because
//! `album`, `artist` and `genre` are profile-wide tables shared across
//! libraries.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{error::AppResult, state::AppState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumRow {
    pub id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_path: Option<String>,
}

/// Private SQL row — the public `AlbumRow` derives `artwork_path` from the
/// per-profile data dir in Rust.
#[derive(FromRow)]
struct AlbumRawRow {
    id: i64,
    title: String,
    artist_name: Option<String>,
    year: Option<i64>,
    track_count: i64,
    total_duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ArtistRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub album_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct GenreRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct FolderRow {
    pub id: i64,
    pub path: String,
    pub last_scanned_at: Option<i64>,
    pub is_watched: i64,
    pub track_count: i64,
}

/// Profile-wide counters shown in the sidebar "Playlists" section.
/// Computed on demand; cheap enough to refetch on every
/// `player:track-changed` event.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileStats {
    pub liked_count: i64,
    pub recent_plays_count: i64,
}

/// Return the count of liked tracks and distinct recently-played
/// tracks (applying the same 15 s / completed filter as
/// [`list_recent_plays`] so the numbers stay in sync with the
/// view).
#[tauri::command]
pub async fn get_profile_stats(
    state: tauri::State<'_, AppState>,
) -> AppResult<ProfileStats> {
    let pool = state.require_profile_pool().await?;

    let liked_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM liked_track")
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

    let recent_plays_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT track_id) FROM play_event
          WHERE completed = 1 OR listened_ms >= 15000",
    )
    .fetch_one(&pool)
    .await
    .unwrap_or(0);

    Ok(ProfileStats {
        liked_count,
        recent_plays_count,
    })
}

/// Row shape returned by `list_recent_plays` — one deduplicated
/// entry per track with its most recent play timestamp. `played_at`
/// and `artwork_path` are resolved post-query.
#[derive(Debug, Clone, Serialize)]
pub struct RecentPlay {
    pub track_id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub duration_ms: i64,
    pub played_at: i64,
    pub artwork_path: Option<String>,
}

/// Internal row shape — the SQL query returns the artwork hash and
/// format separately, and the Rust code resolves the absolute path
/// using the active profile's artwork directory.
#[derive(FromRow)]
struct RecentPlayRaw {
    track_id: i64,
    title: String,
    artist_name: Option<String>,
    album_title: Option<String>,
    duration_ms: i64,
    played_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

/// List every album that has at least one available track in the given
/// library, sorted by artist → album title. Track count and total duration
/// are computed on the fly so the UI can display "Album · N titres · h:mm".
#[tauri::command]
pub async fn list_albums(
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<Vec<AlbumRow>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let raw = sqlx::query_as::<_, AlbumRawRow>(
        r#"
        SELECT al.id,
               al.title,
               ar.name AS artist_name,
               al.year,
               COUNT(t.id)                     AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash                         AS artwork_hash,
               aw.format                       AS artwork_format
          FROM album al
          JOIN track t        ON t.album_id = al.id
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE t.library_id = ? AND t.is_available = 1
         GROUP BY al.id
         ORDER BY ar.canonical_name COLLATE NOCASE,
                  al.canonical_title COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .fetch_all(&pool)
    .await?;

    let rows = raw
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
            AlbumRow {
                id: row.id,
                title: row.title,
                artist_name: row.artist_name,
                year: row.year,
                track_count: row.track_count,
                total_duration_ms: row.total_duration_ms,
                artwork_path,
            }
        })
        .collect();

    Ok(rows)
}

/// List every primary artist that has at least one available track in the
/// given library, with track and album counts.
#[tauri::command]
pub async fn list_artists(
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<Vec<ArtistRow>> {
    let pool = state.require_profile_pool().await?;

    let rows = sqlx::query_as::<_, ArtistRow>(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count
          FROM artist ar
          JOIN track t ON t.primary_artist = ar.id
         WHERE t.library_id = ? AND t.is_available = 1
         GROUP BY ar.id
         ORDER BY ar.canonical_name COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .fetch_all(&pool)
    .await?;

    Ok(rows)
}

/// List every genre that tags at least one available track in the given
/// library, with a track count.
#[tauri::command]
pub async fn list_genres(
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<Vec<GenreRow>> {
    let pool = state.require_profile_pool().await?;

    let rows = sqlx::query_as::<_, GenreRow>(
        r#"
        SELECT g.id,
               g.name,
               COUNT(DISTINCT t.id) AS track_count
          FROM genre g
          JOIN track_genre tg ON tg.genre_id = g.id
          JOIN track t         ON t.id = tg.track_id
         WHERE t.library_id = ? AND t.is_available = 1
         GROUP BY g.id
         ORDER BY g.canonical_name COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .fetch_all(&pool)
    .await?;

    Ok(rows)
}

/// List the most-recently-played tracks for a library, deduplicated
/// to one entry per track (taking the max `played_at` across all
/// `play_event` rows for that track). Used by the "Récemment joués"
/// view in the sidebar.
#[tauri::command]
pub async fn list_recent_plays(
    state: tauri::State<'_, AppState>,
    library_id: i64,
    limit: i64,
) -> AppResult<Vec<RecentPlay>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let artwork_dir =
        profile_id.map(|pid| state.paths.profile_artwork_dir(pid));

    // Only count "real" plays: either the track ran to completion,
    // or the user listened to at least 15 s of it. Without this
    // threshold, every failed / skipped track from past sessions
    // leaves a listened_ms = 0 row that pollutes the view.
    let raw = sqlx::query_as::<_, RecentPlayRaw>(
        r#"
        SELECT t.id                         AS track_id,
               t.title                      AS title,
               ar.name                      AS artist_name,
               al.title                     AS album_title,
               t.duration_ms                AS duration_ms,
               MAX(pe.played_at)            AS played_at,
               aw.hash                      AS artwork_hash,
               aw.format                    AS artwork_format
          FROM play_event pe
          JOIN track t        ON t.id = pe.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artist ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE t.library_id = ?
           AND t.is_available = 1
           AND (pe.completed = 1 OR pe.listened_ms >= 15000)
         GROUP BY t.id
         ORDER BY played_at DESC
         LIMIT ?
        "#,
    )
    .bind(library_id)
    .bind(limit)
    .fetch_all(&pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|row| {
            let artwork_path =
                match (row.artwork_hash, row.artwork_format, artwork_dir.as_ref()) {
                    (Some(hash), Some(format), Some(dir)) => Some(
                        dir.join(format!("{hash}.{format}"))
                            .to_string_lossy()
                            .to_string(),
                    ),
                    _ => None,
                };
            RecentPlay {
                track_id: row.track_id,
                title: row.title,
                artist_name: row.artist_name,
                album_title: row.album_title,
                duration_ms: row.duration_ms,
                played_at: row.played_at,
                artwork_path,
            }
        })
        .collect();

    Ok(rows)
}

/// List every folder registered under the given library, along with the
/// number of tracks found inside it at the last scan.
#[tauri::command]
pub async fn list_folders(
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<Vec<FolderRow>> {
    let pool = state.require_profile_pool().await?;

    let rows = sqlx::query_as::<_, FolderRow>(
        r#"
        SELECT lf.id,
               lf.path,
               lf.last_scanned_at,
               lf.is_watched,
               COALESCE(COUNT(t.id), 0) AS track_count
          FROM library_folder lf
          LEFT JOIN track t
            ON t.folder_id = lf.id AND t.is_available = 1
         WHERE lf.library_id = ?
         GROUP BY lf.id
         ORDER BY lf.path COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .fetch_all(&pool)
    .await?;

    Ok(rows)
}
