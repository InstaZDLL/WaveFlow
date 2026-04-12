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
    /// Deezer CDN URL from the `deezer_artist` cache, if the artist
    /// has been enriched at least once.
    pub picture_url: Option<String>,
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
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
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
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
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
    library_id: Option<i64>,
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
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY al.id
         ORDER BY ar.canonical_name COLLATE NOCASE,
                  al.canonical_title COLLATE NOCASE
        "#,
    )
    .bind(library_id)
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
    library_id: Option<i64>,
) -> AppResult<Vec<ArtistRow>> {
    let pool = state.require_profile_pool().await?;

    let rows = sqlx::query_as::<_, ArtistRow>(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count,
               da.picture_url             AS picture_url
          FROM artist ar
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id
          LEFT JOIN deezer_artist da ON da.deezer_id = ar.deezer_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY ar.id
         ORDER BY ar.canonical_name COLLATE NOCASE
        "#,
    )
    .bind(library_id)
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
    library_id: Option<i64>,
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
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY g.id
         ORDER BY g.canonical_name COLLATE NOCASE
        "#,
    )
    .bind(library_id)
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
    library_id: Option<i64>,
    limit: i64,
) -> AppResult<Vec<RecentPlay>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let artwork_dir =
        profile_id.map(|pid| state.paths.profile_artwork_dir(pid));

    let raw = sqlx::query_as::<_, RecentPlayRaw>(
        r#"
        SELECT t.id                         AS track_id,
               t.title                      AS title,
               t.primary_artist             AS artist_id,
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
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
           AND (pe.completed = 1 OR pe.listened_ms >= 15000)
         GROUP BY t.id
         ORDER BY played_at DESC
         LIMIT ?
        "#,
    )
    .bind(library_id)
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
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
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
    library_id: Option<i64>,
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
         WHERE (? IS NULL OR lf.library_id = ?)
         GROUP BY lf.id
         ORDER BY lf.path COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .fetch_all(&pool)
    .await?;

    Ok(rows)
}

// ── Album detail ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AlbumDetail {
    pub id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_path: Option<String>,
    pub label: Option<String>,
    pub release_date: Option<String>,
    pub genres: Vec<String>,
    pub tracks: Vec<AlbumTrack>,
}

#[derive(FromRow)]
struct AlbumDetailRaw {
    id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    year: Option<i64>,
    release_date: Option<String>,
    track_count: i64,
    total_duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlbumTrack {
    pub id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub artwork_path: Option<String>,
    pub file_path: String,
}

#[derive(FromRow)]
struct AlbumTrackRaw {
    id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    duration_ms: i64,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    file_path: String,
}

/// Return full album detail: header (with Deezer-cached label), genres,
/// and tracks ordered by disc then track number.
#[tauri::command]
pub async fn get_album_detail(
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<AlbumDetail> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let header = sqlx::query_as::<_, AlbumDetailRaw>(
        r#"
        SELECT al.id, al.title, al.artist_id, ar.name AS artist_name,
               al.year, al.release_date,
               COUNT(t.id) AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash AS artwork_hash, aw.format AS artwork_format,
               da.label
          FROM album al
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
          LEFT JOIN deezer_album da ON da.deezer_id = al.deezer_id
          JOIN track t ON t.album_id = al.id AND t.is_available = 1
         WHERE al.id = ?
         GROUP BY al.id
        "#,
    )
    .bind(album_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| crate::error::AppError::Other("album not found".into()))?;

    let artwork_path = match (header.artwork_hash.as_deref(), header.artwork_format.as_deref()) {
        (Some(hash), Some(format)) => Some(
            artwork_dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string(),
        ),
        _ => None,
    };

    let genres: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT DISTINCT g.name
          FROM genre g
          JOIN track_genre tg ON tg.genre_id = g.id
          JOIN track t ON t.id = tg.track_id
         WHERE t.album_id = ?
         ORDER BY g.name COLLATE NOCASE
        "#,
    )
    .bind(album_id)
    .fetch_all(&pool)
    .await?;

    let tracks_raw = sqlx::query_as::<_, AlbumTrackRaw>(
        r#"
        SELECT t.id, t.title,
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
               t.duration_ms, t.track_number, t.disc_number,
               t.file_path,
               aw.hash AS artwork_hash, aw.format AS artwork_format
          FROM track t
          LEFT JOIN album al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE t.album_id = ? AND t.is_available = 1
         ORDER BY t.disc_number, t.track_number
        "#,
    )
    .bind(album_id)
    .fetch_all(&pool)
    .await?;

    let tracks = tracks_raw
        .into_iter()
        .map(|row| {
            let track_artwork = match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                (Some(hash), Some(fmt)) => Some(
                    artwork_dir
                        .join(format!("{hash}.{fmt}"))
                        .to_string_lossy()
                        .to_string(),
                ),
                _ => None,
            };
            AlbumTrack {
                id: row.id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                duration_ms: row.duration_ms,
                track_number: row.track_number,
                disc_number: row.disc_number,
                artwork_path: track_artwork,
                file_path: row.file_path,
            }
        })
        .collect();

    Ok(AlbumDetail {
        id: header.id,
        title: header.title,
        artist_id: header.artist_id,
        artist_name: header.artist_name,
        year: header.year,
        track_count: header.track_count,
        total_duration_ms: header.total_duration_ms,
        artwork_path,
        label: header.label,
        release_date: header.release_date,
        genres,
        tracks,
    })
}

// ── Artist detail ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ArtistDetail {
    pub id: i64,
    pub name: String,
    pub artwork_path: Option<String>,
    pub picture_url: Option<String>,
    pub fans_count: Option<i64>,
    pub track_count: i64,
    pub album_count: i64,
    pub albums: Vec<ArtistAlbumRow>,
}

#[derive(FromRow)]
struct ArtistDetailRaw {
    id: i64,
    name: String,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    picture_url: Option<String>,
    fans_count: Option<i64>,
    track_count: i64,
    album_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtistAlbumRow {
    pub id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_path: Option<String>,
}

#[derive(FromRow)]
struct ArtistAlbumRawRow {
    id: i64,
    title: String,
    year: Option<i64>,
    track_count: i64,
    total_duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

/// Return full artist detail: header, discography, and track count.
#[tauri::command]
pub async fn get_artist_detail(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<ArtistDetail> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let header = sqlx::query_as::<_, ArtistDetailRaw>(
        r#"
        SELECT ar.id, ar.name,
               aw.hash AS artwork_hash, aw.format AS artwork_format,
               da.picture_url AS picture_url,
               da.fans_count  AS fans_count,
               COUNT(DISTINCT t.id) AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count
          FROM artist ar
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN deezer_artist da ON da.deezer_id = ar.deezer_id
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id AND t.is_available = 1
         WHERE ar.id = ?
         GROUP BY ar.id
        "#,
    )
    .bind(artist_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| crate::error::AppError::Other("artist not found".into()))?;

    let artwork_path = match (header.artwork_hash.as_deref(), header.artwork_format.as_deref()) {
        (Some(hash), Some(format)) => Some(
            artwork_dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string(),
        ),
        _ => None,
    };

    let albums_raw = sqlx::query_as::<_, ArtistAlbumRawRow>(
        r#"
        SELECT al.id, al.title, al.year,
               COUNT(DISTINCT t.id) AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash AS artwork_hash, aw.format AS artwork_format
          FROM album al
          JOIN track t ON t.album_id = al.id AND t.is_available = 1
          JOIN track_artist ta ON ta.track_id = t.id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE ta.artist_id = ?
         GROUP BY al.id
         ORDER BY al.year DESC, al.canonical_title COLLATE NOCASE
        "#,
    )
    .bind(artist_id)
    .fetch_all(&pool)
    .await?;

    let albums = albums_raw
        .into_iter()
        .map(|row| {
            let album_artwork = match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                (Some(hash), Some(fmt)) => Some(
                    artwork_dir
                        .join(format!("{hash}.{fmt}"))
                        .to_string_lossy()
                        .to_string(),
                ),
                _ => None,
            };
            ArtistAlbumRow {
                id: row.id,
                title: row.title,
                year: row.year,
                track_count: row.track_count,
                total_duration_ms: row.total_duration_ms,
                artwork_path: album_artwork,
            }
        })
        .collect();

    Ok(ArtistDetail {
        id: header.id,
        name: header.name,
        artwork_path,
        picture_url: header.picture_url,
        fans_count: header.fans_count,
        track_count: header.track_count,
        album_count: header.album_count,
        albums,
    })
}
