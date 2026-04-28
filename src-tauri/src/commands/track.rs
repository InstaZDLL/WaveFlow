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
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    /// Comma-joined artist IDs in the same order as `artist_name`'s
    /// `", "`-joined names. Used by the frontend `ArtistLink` to make
    /// each name individually clickable.
    pub artist_ids: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    /// Bits per sample. `None` for lossy codecs that don't expose
    /// it; populated for FLAC/WAV/AIFF and similar lossless masters.
    pub bit_depth: Option<i64>,
    /// Short codec / container label (`"FLAC"`, `"MP3"`, …). Drives
    /// the format chip on the player footer.
    pub codec: Option<String>,
    pub file_path: String,
    pub file_size: i64,
    pub added_at: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    /// Raw POPM byte (0-255). `None` when no rating was extracted from
    /// the file's tags or set by the user. The frontend converts this
    /// to a 0-5 star scale with half-step increments.
    pub rating: Option<i64>,
}

/// Raw row shape as it comes out of the SQL query — kept private because the
/// public `Track` struct adds a derived `artwork_path` that the database
/// doesn't know how to compute.
#[derive(FromRow)]
struct TrackRow {
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

/// Resolve a sort spec to a SQL `ORDER BY` clause. Whitelisted columns
/// only — never interpolate user input directly. Returns the default
/// "Artist → Album → Disc → Track" ordering when the spec is invalid
/// or absent.
fn track_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(
        order_by,
        Some("rating") | Some("duration_ms") | Some("added_at") | Some("year"),
    );
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
        (Some("title"), "ASC") => "ORDER BY t.title COLLATE NOCASE ASC",
        (Some("title"), "DESC") => "ORDER BY t.title COLLATE NOCASE DESC",
        (Some("artist"), "ASC") => "ORDER BY ar.canonical_name COLLATE NOCASE ASC, t.title COLLATE NOCASE",
        (Some("artist"), "DESC") => "ORDER BY ar.canonical_name COLLATE NOCASE DESC, t.title COLLATE NOCASE",
        (Some("album"), "ASC") => "ORDER BY al.canonical_title COLLATE NOCASE ASC, t.disc_number, t.track_number",
        (Some("album"), "DESC") => "ORDER BY al.canonical_title COLLATE NOCASE DESC, t.disc_number, t.track_number",
        (Some("duration_ms"), "ASC") => "ORDER BY t.duration_ms ASC",
        (Some("duration_ms"), "DESC") => "ORDER BY t.duration_ms DESC",
        (Some("year"), "ASC") => "ORDER BY t.year ASC, t.title COLLATE NOCASE",
        (Some("year"), "DESC") => "ORDER BY t.year DESC, t.title COLLATE NOCASE",
        (Some("added_at"), "ASC") => "ORDER BY t.added_at ASC",
        (Some("added_at"), "DESC") => "ORDER BY t.added_at DESC",
        (Some("rating"), "ASC") => "ORDER BY t.rating ASC, t.title COLLATE NOCASE",
        (Some("rating"), "DESC") => "ORDER BY t.rating DESC, t.title COLLATE NOCASE",
        _ => {
            "ORDER BY ar.canonical_name COLLATE NOCASE,\n                  al.canonical_title COLLATE NOCASE,\n                  t.disc_number,\n                  t.track_number,\n                  t.title COLLATE NOCASE"
        }
    }
}

/// List tracks. When `library_id` is `Some`, only tracks from that library
/// are returned. When `None`, tracks across **all** libraries are shown —
/// the "Ma musique" mode where the concept of multiple libraries is hidden
/// from the user.
///
/// `order_by` and `direction` are whitelisted in [`track_order_clause`]
/// to keep this command injection-free.
#[tauri::command]
pub async fn list_tracks(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<Vec<Track>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = track_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
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
          FROM track t
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         {order_clause}
        "#
    );

    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(library_id)
        .bind(library_id)
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

/// Set or clear a track's rating. The value is the raw POPM byte (0-255);
/// passing `None` clears the rating. Currently only writes to the database
/// — write-back into the file's tags is deferred to a later iteration.
#[tauri::command]
pub async fn set_track_rating(
    state: tauri::State<'_, AppState>,
    track_id: i64,
    rating: Option<u8>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    sqlx::query("UPDATE track SET rating = ? WHERE id = ?")
        .bind(rating.map(|r| r as i64))
        .bind(track_id)
        .execute(&pool)
        .await?;
    Ok(())
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
