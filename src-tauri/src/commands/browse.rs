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
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    /// Best-quality bit depth across the album's tracks. Drives the
    /// Hi-Res cover badge — if any track in the album is mastered at
    /// 24-bit, the badge shows on the cover. `None` when no track
    /// has a known bit depth (e.g. all MP3s).
    pub max_bit_depth: Option<i64>,
    pub max_sample_rate: Option<i64>,
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
    max_bit_depth: Option<i64>,
    max_sample_rate: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub album_count: i64,
    /// Deezer CDN URL from the `metadata_artist` cache, if the artist
    /// has been enriched at least once. Kept as a fallback — the UI
    /// should prefer `picture_path` when present.
    pub picture_url: Option<String>,
    /// Absolute filesystem path to the locally-cached picture, when
    /// the metadata cache holds a hash and the file still exists.
    pub picture_path: Option<String>,
    pub picture_path_1x: Option<String>,
    pub picture_path_2x: Option<String>,
}

#[derive(FromRow)]
struct ArtistRowRaw {
    id: i64,
    name: String,
    track_count: i64,
    album_count: i64,
    picture_url: Option<String>,
    picture_hash: Option<String>,
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
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub duration_ms: i64,
    pub played_at: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub file_path: String,
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
    album_id: Option<i64>,
    album_title: Option<String>,
    duration_ms: i64,
    played_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    file_path: String,
}

/// Whitelisted ORDER BY clause builder for `list_albums`. Falls back to
/// the default "Artist → Album" sort whenever the spec isn't recognized.
fn album_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(order_by, Some("year") | Some("added_at"));
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("title"), "ASC") => "ORDER BY al.canonical_title COLLATE NOCASE ASC",
        (Some("title"), "DESC") => "ORDER BY al.canonical_title COLLATE NOCASE DESC",
        (Some("artist"), "ASC") => "ORDER BY ar.canonical_name COLLATE NOCASE ASC, al.canonical_title COLLATE NOCASE",
        (Some("artist"), "DESC") => "ORDER BY ar.canonical_name COLLATE NOCASE DESC, al.canonical_title COLLATE NOCASE",
        (Some("year"), "ASC") => "ORDER BY al.year ASC, al.canonical_title COLLATE NOCASE",
        (Some("year"), "DESC") => "ORDER BY al.year DESC, al.canonical_title COLLATE NOCASE",
        (Some("added_at"), "ASC") => "ORDER BY MIN(t.added_at) ASC",
        (Some("added_at"), "DESC") => "ORDER BY MIN(t.added_at) DESC",
        _ => "ORDER BY ar.canonical_name COLLATE NOCASE,\n                  al.canonical_title COLLATE NOCASE",
    }
}

/// Whitelisted ORDER BY clause builder for `list_artists`.
fn artist_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc =
        matches!(order_by, Some("albums_count") | Some("tracks_count"));
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("name"), "ASC") => "ORDER BY ar.canonical_name COLLATE NOCASE ASC",
        (Some("name"), "DESC") => "ORDER BY ar.canonical_name COLLATE NOCASE DESC",
        (Some("albums_count"), "ASC") => "ORDER BY album_count ASC, ar.canonical_name COLLATE NOCASE",
        (Some("albums_count"), "DESC") => "ORDER BY album_count DESC, ar.canonical_name COLLATE NOCASE",
        (Some("tracks_count"), "ASC") => "ORDER BY track_count ASC, ar.canonical_name COLLATE NOCASE",
        (Some("tracks_count"), "DESC") => "ORDER BY track_count DESC, ar.canonical_name COLLATE NOCASE",
        _ => "ORDER BY ar.canonical_name COLLATE NOCASE",
    }
}

/// List every album that has at least one available track in the given
/// library, sorted by artist → album title. Track count and total duration
/// are computed on the fly so the UI can display "Album · N titres · h:mm".
#[tauri::command]
pub async fn list_albums(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    filter_no_cover: Option<bool>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<Vec<AlbumRow>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let no_cover = filter_no_cover.unwrap_or(false);
    let order_clause = album_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT al.id,
               al.title,
               ar.name AS artist_name,
               al.year,
               COUNT(t.id)                     AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash                         AS artwork_hash,
               aw.format                       AS artwork_format,
               MAX(t.bit_depth)                AS max_bit_depth,
               MAX(t.sample_rate)              AS max_sample_rate
          FROM album al
          JOIN track t        ON t.album_id = al.id
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
           AND (? = 0 OR al.artwork_id IS NULL)
         GROUP BY al.id
         {order_clause}
"#
    );

    let raw = sqlx::query_as::<_, AlbumRawRow>(&sql)
        .bind(library_id)
        .bind(library_id)
        .bind(if no_cover { 1_i64 } else { 0_i64 })
        .fetch_all(&pool)
        .await?;

    let rows = raw
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
            AlbumRow {
                id: row.id,
                title: row.title,
                artist_name: row.artist_name,
                year: row.year,
                track_count: row.track_count,
                total_duration_ms: row.total_duration_ms,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
                max_bit_depth: row.max_bit_depth,
                max_sample_rate: row.max_sample_rate,
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
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<Vec<ArtistRow>> {
    let pool = state.require_profile_pool().await?;

    let order_clause = artist_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count,
               da.picture_url             AS picture_url,
               da.picture_hash            AS picture_hash
          FROM artist ar
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY ar.id
         {order_clause}
        "#
    );

    let raw = sqlx::query_as::<_, ArtistRowRaw>(&sql)
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&pool)
        .await?;

    let metadata_dir = &state.paths.metadata_artwork_dir;
    let rows = raw
        .into_iter()
        .map(|r| {
            let (picture_path_1x, picture_path_2x) = match r.picture_hash.as_deref() {
                Some(h) => crate::thumbnails::thumbnail_paths_for(metadata_dir, h),
                None => (None, None),
            };
            ArtistRow {
                id: r.id,
                name: r.name,
                track_count: r.track_count,
                album_count: r.album_count,
                picture_path: r
                    .picture_hash
                    .as_deref()
                    .and_then(|h| crate::metadata_artwork::existing_path(metadata_dir, h)),
                picture_url: r.picture_url,
                picture_path_1x,
                picture_path_2x,
            }
        })
        .collect();

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
               t.album_id                   AS album_id,
               al.title                     AS album_title,
               t.duration_ms                AS duration_ms,
               MAX(pe.played_at)            AS played_at,
               aw.hash                      AS artwork_hash,
               aw.format                    AS artwork_format,
               t.file_path                  AS file_path
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
            let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
                row.artwork_hash.as_deref(),
                row.artwork_format.as_deref(),
                artwork_dir.as_ref(),
            ) {
                (Some(hash), Some(format), Some(dir)) => {
                    let full = dir
                        .join(format!("{hash}.{format}"))
                        .to_string_lossy()
                        .to_string();
                    let (p1, p2) = crate::thumbnails::thumbnail_paths_for(dir, hash);
                    (Some(full), p1, p2)
                }
                _ => (None, None, None),
            };
            RecentPlay {
                track_id: row.track_id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                album_id: row.album_id,
                album_title: row.album_title,
                duration_ms: row.duration_ms,
                played_at: row.played_at,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
                file_path: row.file_path,
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
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
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
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub file_path: String,
    /// Per-track quality fields surfaced for the inline Hi-Res
    /// badge on the AlbumDetailView track list.
    pub bit_depth: Option<i64>,
    pub sample_rate: Option<i64>,
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
    bit_depth: Option<i64>,
    sample_rate: Option<i64>,
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
          LEFT JOIN app.metadata_album da ON da.deezer_id = al.deezer_id
          JOIN track t ON t.album_id = al.id AND t.is_available = 1
         WHERE al.id = ?
         GROUP BY al.id
        "#,
    )
    .bind(album_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| crate::error::AppError::Other("album not found".into()))?;

    let (artwork_path, artwork_path_1x, artwork_path_2x) =
        match (header.artwork_hash.as_deref(), header.artwork_format.as_deref()) {
            (Some(hash), Some(format)) => {
                let full = artwork_dir
                    .join(format!("{hash}.{format}"))
                    .to_string_lossy()
                    .to_string();
                let (p1, p2) =
                    crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                (Some(full), p1, p2)
            }
            _ => (None, None, None),
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
               t.bit_depth, t.sample_rate,
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
            let (track_artwork, track_artwork_1x, track_artwork_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(fmt)) => {
                        let full = artwork_dir
                            .join(format!("{hash}.{fmt}"))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) =
                            crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
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
                artwork_path_1x: track_artwork_1x,
                artwork_path_2x: track_artwork_2x,
                file_path: row.file_path,
                bit_depth: row.bit_depth,
                sample_rate: row.sample_rate,
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
        artwork_path_1x,
        artwork_path_2x,
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
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub picture_url: Option<String>,
    pub picture_path: Option<String>,
    pub picture_path_1x: Option<String>,
    pub picture_path_2x: Option<String>,
    pub fans_count: Option<i64>,
    pub bio_short: Option<String>,
    pub bio_full: Option<String>,
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
    picture_hash: Option<String>,
    fans_count: Option<i64>,
    bio_short: Option<String>,
    bio_full: Option<String>,
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
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
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
               da.picture_url  AS picture_url,
               da.picture_hash AS picture_hash,
               da.fans_count   AS fans_count,
               da.bio_short    AS bio_short,
               da.bio_full     AS bio_full,
               COUNT(DISTINCT t.id) AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count
          FROM artist ar
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
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

    let (artwork_path, artwork_path_1x, artwork_path_2x) =
        match (header.artwork_hash.as_deref(), header.artwork_format.as_deref()) {
            (Some(hash), Some(format)) => {
                let full = artwork_dir
                    .join(format!("{hash}.{format}"))
                    .to_string_lossy()
                    .to_string();
                let (p1, p2) =
                    crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                (Some(full), p1, p2)
            }
            _ => (None, None, None),
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
            let (album_artwork, album_artwork_1x, album_artwork_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(fmt)) => {
                        let full = artwork_dir
                            .join(format!("{hash}.{fmt}"))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) =
                            crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            ArtistAlbumRow {
                id: row.id,
                title: row.title,
                year: row.year,
                track_count: row.track_count,
                total_duration_ms: row.total_duration_ms,
                artwork_path: album_artwork,
                artwork_path_1x: album_artwork_1x,
                artwork_path_2x: album_artwork_2x,
            }
        })
        .collect();

    let metadata_dir = &state.paths.metadata_artwork_dir;
    let picture_path = header
        .picture_hash
        .as_deref()
        .and_then(|h| crate::metadata_artwork::existing_path(metadata_dir, h));
    let (picture_path_1x, picture_path_2x) = match header.picture_hash.as_deref() {
        Some(h) => crate::thumbnails::thumbnail_paths_for(metadata_dir, h),
        None => (None, None),
    };

    Ok(ArtistDetail {
        id: header.id,
        name: header.name,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
        picture_url: header.picture_url,
        picture_path,
        picture_path_1x,
        picture_path_2x,
        fans_count: header.fans_count,
        bio_short: header.bio_short,
        bio_full: header.bio_full,
        track_count: header.track_count,
        album_count: header.album_count,
        albums,
    })
}
