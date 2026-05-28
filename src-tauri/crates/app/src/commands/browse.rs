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

/// Slim album row shipped by `list_albums` — artwork is represented by
/// `(hash, format, has_1x, has_2x)` so the response-level
/// `artwork_base` carries the per-profile prefix once instead of
/// repeating it on every row.
#[derive(Debug, Clone, Serialize)]
pub struct AlbumRow {
    pub id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    /// Best-quality bit depth across the album's tracks. Drives the
    /// Hi-Res cover badge — if any track in the album is mastered at
    /// 24-bit, the badge shows on the cover. `None` when no track
    /// has a known bit depth (e.g. all MP3s).
    pub max_bit_depth: Option<i64>,
    pub max_sample_rate: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListAlbumsResponse {
    /// Per-profile artwork dir. Stitch `<base>/<hash>.<format>` for the
    /// full image, `<base>/<hash>_1x.jpg` / `<base>/<hash>_2x.jpg` for
    /// thumbnails (the thumbnail pipeline always emits JPEG regardless
    /// of the source extension).
    pub artwork_base: String,
    pub items: Vec<AlbumRow>,
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

/// Slim artist row — same wire-format contract as `AlbumRow`. Two
/// hash families (local `artwork_*` and Deezer-cached `picture_*`)
/// because the UI prefers the extracted local image and only falls
/// back to the Deezer cache when the local one is missing.
#[derive(Debug, Clone, Serialize)]
pub struct ArtistRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub album_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    /// Cached-Deezer picture hash. Files are stored under the shared
    /// `metadata_artwork_base`, always as `<hash>.jpg`.
    pub picture_hash: Option<String>,
    pub picture_has_1x: bool,
    pub picture_has_2x: bool,
    /// Deezer CDN URL — last-resort fallback when no local file is
    /// available (e.g. when the cache was wiped or the picture is on a
    /// remote profile being browsed offline).
    pub picture_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListArtistsResponse {
    pub artwork_base: String,
    pub metadata_artwork_base: String,
    pub items: Vec<ArtistRow>,
}

#[derive(FromRow)]
struct ArtistRowRaw {
    id: i64,
    name: String,
    track_count: i64,
    album_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
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
pub async fn get_profile_stats(state: tauri::State<'_, AppState>) -> AppResult<ProfileStats> {
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
    let dir_default_desc = matches!(order_by, Some("albums_count") | Some("tracks_count"));
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
        (Some("albums_count"), "ASC") => {
            "ORDER BY album_count ASC, ar.canonical_name COLLATE NOCASE"
        }
        (Some("albums_count"), "DESC") => {
            "ORDER BY album_count DESC, ar.canonical_name COLLATE NOCASE"
        }
        (Some("tracks_count"), "ASC") => {
            "ORDER BY track_count ASC, ar.canonical_name COLLATE NOCASE"
        }
        (Some("tracks_count"), "DESC") => {
            "ORDER BY track_count DESC, ar.canonical_name COLLATE NOCASE"
        }
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
) -> AppResult<ListAlbumsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let no_cover = filter_no_cover.unwrap_or(false);
    let order_clause = album_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT al.id,
               al.title,
               COALESCE(ar.name, al.album_artist) AS artist_name,
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

    let raw = sqlx::query_as::<_, AlbumRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .bind(if no_cover { 1_i64 } else { 0_i64 })
        .fetch_all(&pool)
        .await?;

    // Per-row mapping does N synchronous `Path::exists` probes against
    // the artwork dir (via `thumbnail_paths_for`). At 850+ albums × 2
    // checks that's enough sustained syscalls to noticeably stall the
    // tokio runtime, so we hand the whole batch off to the blocking
    // pool in one shot — single hop, no per-row overhead.
    let artwork_dir_for_blocking = artwork_dir.clone();
    let items = tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|row| {
                let (artwork_has_1x, artwork_has_2x) = match row.artwork_hash.as_deref() {
                    Some(hash) => {
                        let (p1, p2) =
                            crate::thumbnails::thumbnail_paths_for(&artwork_dir_for_blocking, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                AlbumRow {
                    id: row.id,
                    title: row.title,
                    artist_name: row.artist_name,
                    year: row.year,
                    track_count: row.track_count,
                    total_duration_ms: row.total_duration_ms,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    max_bit_depth: row.max_bit_depth,
                    max_sample_rate: row.max_sample_rate,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| crate::error::AppError::Other(format!("list_albums join: {e}")))?;

    Ok(ListAlbumsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// List every primary artist that has at least one available track in the
/// given library, with track and album counts.
#[tauri::command]
pub async fn list_artists(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListArtistsResponse> {
    let pool = state.require_profile_pool().await?;

    let order_clause = artist_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count,
               aw.hash                    AS artwork_hash,
               aw.format                  AS artwork_format,
               da.picture_url             AS picture_url,
               da.picture_hash            AS picture_hash
          FROM artist ar
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY ar.id
         {order_clause}
        "#
    );

    let raw = sqlx::query_as::<_, ArtistRowRaw>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&pool)
        .await?;

    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();
    // Same blocking-pool offload as `list_albums`: each row triggers up
    // to 5 `Path::exists` probes (1 Deezer-full + 2 local thumbs + 2
    // Deezer thumbs) — at 900 artists that's ~4 500 syscalls in a
    // tight loop, well past the threshold where stalling the tokio
    // runtime starts to matter.
    let artwork_dir_for_blocking = artwork_dir.clone();
    let metadata_dir_for_blocking = metadata_dir.clone();
    let items = tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|r| {
                let (artwork_has_1x, artwork_has_2x) = match r.artwork_hash.as_deref() {
                    Some(hash) => {
                        let (p1, p2) =
                            crate::thumbnails::thumbnail_paths_for(&artwork_dir_for_blocking, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                // For the Deezer cache the "full" file uses the same
                // `<hash>.jpg` naming pattern, so we can drop a `picture_hash`
                // when the source file is missing — the frontend won't have
                // anything to point a thumbnail variant at either.
                let picture_hash = r.picture_hash.and_then(|h| {
                    if crate::metadata_artwork::existing_path(&metadata_dir_for_blocking, &h)
                        .is_some()
                    {
                        Some(h)
                    } else {
                        None
                    }
                });
                let (picture_has_1x, picture_has_2x) = match picture_hash.as_deref() {
                    Some(h) => {
                        let (p1, p2) =
                            crate::thumbnails::thumbnail_paths_for(&metadata_dir_for_blocking, h);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                ArtistRow {
                    id: r.id,
                    name: r.name,
                    track_count: r.track_count,
                    album_count: r.album_count,
                    artwork_hash: r.artwork_hash,
                    artwork_format: r.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    picture_hash,
                    picture_has_1x,
                    picture_has_2x,
                    picture_url: r.picture_url,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| crate::error::AppError::Other(format!("list_artists join: {e}")))?;

    Ok(ListArtistsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
        items,
    })
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
    let artwork_dir = profile_id.map(|pid| state.paths.profile_artwork_dir(pid));

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
    /// Codec label from the scanner (e.g. "FLAC", "MP3", "DSD128").
    /// Lets the inline Hi-Res badge swap to a "DSD64/128/…" label
    /// for DSF/DFF tracks where bit_depth=1 would otherwise look
    /// like junk to the badge logic.
    pub codec: Option<String>,
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
    codec: Option<String>,
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
        SELECT al.id, al.title, al.artist_id,
               COALESCE(ar.name, al.album_artist) AS artist_name,
               al.year, al.release_date,
               aw.hash AS artwork_hash, aw.format AS artwork_format,
               da.label
          FROM album al
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
          LEFT JOIN app.metadata_album da ON da.deezer_id = al.deezer_id
         WHERE al.id = ?
        "#,
    )
    .bind(album_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| crate::error::AppError::Other("album not found".into()))?;

    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
        header.artwork_hash.as_deref(),
        header.artwork_format.as_deref(),
    ) {
        (Some(hash), Some(format)) => {
            let full = artwork_dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string();
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
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

    // Collapse duplicate files (same album/disc/track_number) — e.g. when the
    // same song was scanned in both FLAC and MP3 form. We keep the highest-
    // quality variant per slot: bit_depth desc, then sample_rate desc, then
    // file_size desc, then id asc as a stable tie-breaker. Tracks without a
    // track_number get their own slot (-id) so they're never collapsed
    // together blindly.
    //
    // DSD nuance: DSF/DFF tracks report `bit_depth = 1` (one bit per sample,
    // not 1-bit lossy). A naïve `bit_depth DESC` would rank them BELOW
    // 16-bit MP3, dropping the higher-quality DSD variant from a mixed
    // DSD/PCM album. The first sort key promotes `bit_depth = 1` rows ahead
    // of every PCM row so DSD always wins the collapse when present.
    let tracks_raw = sqlx::query_as::<_, AlbumTrackRaw>(
        r#"
        WITH ranked AS (
            SELECT t.id,
                   ROW_NUMBER() OVER (
                       PARTITION BY COALESCE(t.disc_number, 1),
                                    COALESCE(t.track_number, -t.id)
                       ORDER BY (t.bit_depth IS NULL),
                                (t.bit_depth = 1) DESC,
                                t.bit_depth DESC,
                                t.sample_rate DESC,
                                t.file_size DESC,
                                t.id ASC
                   ) AS rn
              FROM track t
             WHERE t.album_id = ? AND t.is_available = 1
        )
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
               t.bit_depth, t.sample_rate, t.codec,
               aw.hash AS artwork_hash, aw.format AS artwork_format
          FROM ranked r
          JOIN track t ON t.id = r.id
          LEFT JOIN album al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE r.rn = 1
         ORDER BY t.disc_number, t.track_number
        "#,
    )
    .bind(album_id)
    .fetch_all(&pool)
    .await?;

    let tracks: Vec<AlbumTrack> = tracks_raw
        .into_iter()
        .map(|row| {
            let (track_artwork, track_artwork_1x, track_artwork_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(fmt)) => {
                        let full = artwork_dir
                            .join(format!("{hash}.{fmt}"))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
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
                codec: row.codec,
            }
        })
        .collect();

    let track_count = tracks.len() as i64;
    let total_duration_ms = tracks.iter().map(|t| t.duration_ms).sum();

    Ok(AlbumDetail {
        id: header.id,
        title: header.title,
        artist_id: header.artist_id,
        artist_name: header.artist_name,
        year: header.year,
        track_count,
        total_duration_ms,
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

    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
        header.artwork_hash.as_deref(),
        header.artwork_format.as_deref(),
    ) {
        (Some(hash), Some(format)) => {
            let full = artwork_dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string();
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
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
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
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

// ── Genre detail ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GenreDetail {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub tracks: Vec<crate::commands::track::Track>,
}

#[derive(FromRow)]
struct GenreHeaderRaw {
    id: i64,
    name: String,
}

#[derive(FromRow)]
struct GenreTrackRaw {
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
    musical_key: Option<String>,
    file_path: String,
    file_size: i64,
    added_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    rating: Option<i64>,
}

/// Return full genre detail: header (name, totals) and every track tagged
/// with this genre across the active profile, ordered by artist → album →
/// disc → track number to match `list_tracks`'s default layout.
#[tauri::command]
pub async fn get_genre_detail(
    state: tauri::State<'_, AppState>,
    genre_id: i64,
) -> AppResult<GenreDetail> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let header = sqlx::query_as::<_, GenreHeaderRaw>(r#"SELECT id, name FROM genre WHERE id = ?"#)
        .bind(genre_id)
        .fetch_optional(&pool)
        .await?
        .ok_or_else(|| crate::error::AppError::Other("genre not found".into()))?;

    let rows = sqlx::query_as::<_, GenreTrackRaw>(
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
               t.bit_depth, t.codec, t.musical_key,
               t.file_path, t.file_size, t.added_at,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format,
               t.rating  AS rating
          FROM track t
          JOIN track_genre tg ON tg.track_id = t.id
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE tg.genre_id = ? AND t.is_available = 1
         ORDER BY ar.canonical_name COLLATE NOCASE,
                  al.canonical_title COLLATE NOCASE,
                  t.disc_number,
                  t.track_number,
                  t.title COLLATE NOCASE
        "#,
    )
    .bind(genre_id)
    .fetch_all(&pool)
    .await?;

    let tracks: Vec<crate::commands::track::Track> = rows
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(format)) => {
                        let full = artwork_dir
                            .join(format!("{}.{}", hash, format))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            crate::commands::track::Track {
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
                musical_key: row.musical_key,
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

    let track_count = tracks.len() as i64;
    let total_duration_ms = tracks.iter().map(|t| t.duration_ms).sum();

    Ok(GenreDetail {
        id: header.id,
        name: header.name,
        track_count,
        total_duration_ms,
        tracks,
    })
}

// ─── Play history (Last.fm-style chronological scrubber) ─────────
//
// Distinct from `list_recent_plays`, which deduplicates per track.
// The history view wants every individual play_event as its own row
// so the user can actually see "I played X three times this evening".

/// One row per `play_event` (no per-track dedup), reverse-chronological.
#[derive(Debug, Clone, Serialize)]
pub struct PlayHistoryRow {
    pub event_id: i64,
    pub played_at: i64,
    pub listened_ms: i64,
    pub completed: bool,
    pub track_id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub duration_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub file_path: String,
}

#[derive(FromRow)]
struct PlayHistoryRaw {
    event_id: i64,
    played_at: i64,
    listened_ms: i64,
    completed: i64,
    track_id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    album_id: Option<i64>,
    album_title: Option<String>,
    duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    file_path: String,
}

/// Returns one row per play_event in reverse-chronological order.
/// `before_ms` is an exclusive upper bound on `played_at` — pass the
/// `played_at` of the last row from the previous page to paginate
/// without windowing artefacts when new plays land mid-scroll.
/// `after_ms` is an inclusive lower bound for date-range filtering
/// (e.g. "show me only plays since 2026-01-01"). Both are optional.
#[tauri::command]
pub async fn list_play_history(
    state: tauri::State<'_, AppState>,
    before_ms: Option<i64>,
    after_ms: Option<i64>,
    limit: i64,
) -> AppResult<Vec<PlayHistoryRow>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let artwork_dir = profile_id.map(|pid| state.paths.profile_artwork_dir(pid));

    let raw = sqlx::query_as::<_, PlayHistoryRaw>(
        r#"
        SELECT pe.id                        AS event_id,
               pe.played_at                 AS played_at,
               pe.listened_ms               AS listened_ms,
               pe.completed                 AS completed,
               t.id                         AS track_id,
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
               aw.hash                      AS artwork_hash,
               aw.format                    AS artwork_format,
               t.file_path                  AS file_path
          FROM play_event pe
          JOIN track t        ON t.id = pe.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE t.is_available = 1
           AND (?1 IS NULL OR pe.played_at < ?1)
           AND (?2 IS NULL OR pe.played_at >= ?2)
         ORDER BY pe.played_at DESC, pe.id DESC
         LIMIT ?3
        "#,
    )
    .bind(before_ms)
    .bind(after_ms)
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
            PlayHistoryRow {
                event_id: row.event_id,
                played_at: row.played_at,
                listened_ms: row.listened_ms,
                completed: row.completed != 0,
                track_id: row.track_id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                album_id: row.album_id,
                album_title: row.album_title,
                duration_ms: row.duration_ms,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
                file_path: row.file_path,
            }
        })
        .collect();

    Ok(rows)
}

/// One bucket per (year, month) for the play-history scrubber. Returns
/// the aggregated play count so the UI can render a sparkline-style
/// indicator next to each month label. Sorted oldest → newest because
/// the scrubber renders top-to-bottom and the user expects the latest
/// month at the bottom (next to where the page anchors on first load).
#[derive(Debug, Clone, Serialize)]
pub struct PlayHistoryMonth {
    pub year: i32,
    pub month: u32,
    /// Unix epoch ms at the first instant of this month (UTC).
    pub start_ms: i64,
    pub plays: i64,
}

#[derive(FromRow)]
struct PlayHistoryMonthRaw {
    bucket: String,
    plays: i64,
}

#[tauri::command]
pub async fn play_history_months(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<PlayHistoryMonth>> {
    let pool = state.require_profile_pool().await?;
    let raw = sqlx::query_as::<_, PlayHistoryMonthRaw>(
        r#"
        SELECT strftime('%Y-%m', played_at / 1000, 'unixepoch', 'localtime') AS bucket,
               COUNT(*)                                                       AS plays
          FROM play_event
         GROUP BY bucket
         ORDER BY bucket ASC
        "#,
    )
    .fetch_all(&pool)
    .await?;

    let mut out = Vec::with_capacity(raw.len());
    for r in raw {
        // bucket = "YYYY-MM" — split & convert to (year, month) plus
        // the first-of-month epoch ms. The SQL `strftime(..., 'localtime')`
        // above bucketed by **local** time, so the reconstructed midnight
        // must also be interpreted as local time. Using `and_utc()` here
        // would push the start_ms off by the local UTC offset (visible at
        // a glance as the scrubber showing the wrong month label for
        // events that happened near midnight on the boundary days).
        use chrono::{LocalResult, NaiveDate, TimeZone};
        let mut parts = r.bucket.splitn(2, '-');
        let year: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1970);
        let month: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let start_ms = NaiveDate::from_ymd_opt(year, month, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .and_then(|naive| match chrono::Local.from_local_datetime(&naive) {
                LocalResult::Single(dt) => Some(dt.timestamp_millis()),
                // The first of a month falls inside a DST spring-forward
                // gap (`None`) or fall-back ambiguity (`Ambiguous`) only
                // in vanishingly rare jurisdictions. Pick the earlier
                // interpretation so the scrubber stays monotonic.
                LocalResult::Ambiguous(early, _late) => Some(early.timestamp_millis()),
                LocalResult::None => None,
            })
            .unwrap_or(0);
        out.push(PlayHistoryMonth {
            year,
            month,
            start_ms,
            plays: r.plays,
        });
    }
    Ok(out)
}
