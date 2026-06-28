//! Listening statistics derived from the `play_event` table.
//!
//! Every command takes a `range` in `{"7d","30d","90d","1y","all"}`.
//! The range is translated to a lower bound on `played_at` (epoch ms);
//! `"all"` means no lower bound.
//!
//! All `play_event` rows already passed the 15 s "credit" threshold
//! upstream (see `audio/analytics.rs`), so no extra `listened_ms`
//! filter is needed here. Multi-artist tops join via `track_artist`
//! so featured artists appear in their own right.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

use crate::{error::AppResult, state::AppState};

/// Convert a `range` literal into a UNIX epoch ms lower bound, or
/// `None` when the caller asked for "all". Unknown values fall back
/// to "30d" — keeps the UI safe if the frontend ever sends junk.
fn range_to_since_ms(range: &str) -> Option<i64> {
    let now = chrono::Utc::now().timestamp_millis();
    let day_ms: i64 = 24 * 60 * 60 * 1000;
    let days = match range {
        "7d" => 7,
        "30d" => 30,
        "90d" => 90,
        "1y" => 365,
        "all" => return None,
        _ => 30,
    };
    Some(now - days * day_ms)
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct StatsOverview {
    pub total_plays: i64,
    pub total_ms: i64,
    pub unique_tracks: i64,
    pub unique_artists: i64,
    pub completion_rate: f64,
}

#[tauri::command]
pub async fn stats_overview(
    state: tauri::State<'_, AppState>,
    range: String,
) -> AppResult<StatsOverview> {
    let pool = state.require_profile_pool().await?;
    stats_overview_inner(&pool, &range).await
}

async fn stats_overview_inner(pool: &SqlitePool, range: &str) -> AppResult<StatsOverview> {
    let since = range_to_since_ms(range);

    let row: (i64, i64, i64, Option<f64>) = sqlx::query_as(
        r#"
        SELECT COUNT(*)                    AS total_plays,
               COALESCE(SUM(listened_ms), 0) AS total_ms,
               COUNT(DISTINCT track_id)    AS unique_tracks,
               AVG(completed * 1.0)        AS completion_rate
          FROM play_event
         WHERE (?1 IS NULL OR played_at >= ?1)
        "#,
    )
    .bind(since)
    .fetch_one(pool)
    .await?;

    // Distinct artists requires a join through `track_artist`, so it
    // gets its own query rather than bloating the aggregate above.
    let unique_artists: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(DISTINCT ta.artist_id)
          FROM play_event pe
          JOIN track_artist ta ON ta.track_id = pe.track_id
         WHERE (?1 IS NULL OR pe.played_at >= ?1)
        "#,
    )
    .bind(since)
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    Ok(StatsOverview {
        total_plays: row.0,
        total_ms: row.1,
        unique_tracks: row.2,
        unique_artists,
        completion_rate: row.3.unwrap_or(0.0),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct TopTrackRow {
    pub track_id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub plays: i64,
    pub listened_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
}

#[derive(FromRow)]
struct TopTrackRaw {
    track_id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    album_id: Option<i64>,
    album_title: Option<String>,
    plays: i64,
    listened_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

#[tauri::command]
pub async fn stats_top_tracks(
    state: tauri::State<'_, AppState>,
    range: String,
    limit: i64,
) -> AppResult<Vec<TopTrackRow>> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    stats_top_tracks_inner(&pool, &artwork_dir, &range, limit).await
}

async fn stats_top_tracks_inner(
    pool: &SqlitePool,
    artwork_dir: &Path,
    range: &str,
    limit: i64,
) -> AppResult<Vec<TopTrackRow>> {
    let since = range_to_since_ms(range);

    let raw = sqlx::query_as::<_, TopTrackRaw>(
        r#"
        SELECT t.id                          AS track_id,
               t.title                       AS title,
               t.primary_artist              AS artist_id,
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
               t.album_id                    AS album_id,
               al.title                      AS album_title,
               COUNT(*)                      AS plays,
               COALESCE(SUM(pe.listened_ms), 0) AS listened_ms,
               aw.hash                       AS artwork_hash,
               aw.format                     AS artwork_format
          FROM play_event pe
          JOIN track t       ON t.id = pe.track_id
          LEFT JOIN album al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (?1 IS NULL OR pe.played_at >= ?1)
         GROUP BY t.id
         ORDER BY plays DESC, listened_ms DESC
         LIMIT ?2
        "#,
    )
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(fmt)) => {
                        let full = artwork_dir
                            .join(format!("{hash}.{fmt}"))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            TopTrackRow {
                track_id: row.track_id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                album_id: row.album_id,
                album_title: row.album_title,
                plays: row.plays,
                listened_ms: row.listened_ms,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
            }
        })
        .collect();

    Ok(rows)
}

#[derive(Debug, Clone, Serialize)]
pub struct TopArtistRow {
    pub artist_id: i64,
    pub name: String,
    pub plays: i64,
    pub listened_ms: i64,
    pub picture_url: Option<String>,
    pub picture_path: Option<String>,
    pub picture_path_1x: Option<String>,
    pub picture_path_2x: Option<String>,
}

#[derive(FromRow)]
struct TopArtistRaw {
    artist_id: i64,
    name: String,
    plays: i64,
    listened_ms: i64,
    picture_url: Option<String>,
    picture_hash: Option<String>,
}

#[tauri::command]
pub async fn stats_top_artists(
    state: tauri::State<'_, AppState>,
    range: String,
    limit: i64,
) -> AppResult<Vec<TopArtistRow>> {
    let pool = state.require_profile_pool().await?;
    stats_top_artists_inner(&pool, &state.paths.metadata_artwork_dir, &range, limit).await
}

async fn stats_top_artists_inner(
    pool: &SqlitePool,
    metadata_dir: &Path,
    range: &str,
    limit: i64,
) -> AppResult<Vec<TopArtistRow>> {
    let since = range_to_since_ms(range);

    let raw = sqlx::query_as::<_, TopArtistRaw>(
        r#"
        SELECT ar.id                         AS artist_id,
               ar.name                       AS name,
               COUNT(*)                      AS plays,
               COALESCE(SUM(pe.listened_ms), 0) AS listened_ms,
               da.picture_url                AS picture_url,
               da.picture_hash               AS picture_hash
          FROM play_event pe
          JOIN track_artist ta ON ta.track_id = pe.track_id
          JOIN artist ar       ON ar.id = ta.artist_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE (?1 IS NULL OR pe.played_at >= ?1)
         GROUP BY ar.id
         ORDER BY plays DESC, listened_ms DESC
         LIMIT ?2
        "#,
    )
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|r| {
            let (picture_path_1x, picture_path_2x) = match r.picture_hash.as_deref() {
                Some(h) => crate::thumbnails::thumbnail_paths_for(metadata_dir, h),
                None => (None, None),
            };
            TopArtistRow {
                artist_id: r.artist_id,
                name: r.name,
                plays: r.plays,
                listened_ms: r.listened_ms,
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

#[derive(Debug, Clone, Serialize)]
pub struct TopAlbumRow {
    pub album_id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub plays: i64,
    pub listened_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
}

#[derive(FromRow)]
struct TopAlbumRaw {
    album_id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    plays: i64,
    listened_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

#[tauri::command]
pub async fn stats_top_albums(
    state: tauri::State<'_, AppState>,
    range: String,
    limit: i64,
) -> AppResult<Vec<TopAlbumRow>> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    stats_top_albums_inner(&pool, &artwork_dir, &range, limit).await
}

async fn stats_top_albums_inner(
    pool: &SqlitePool,
    artwork_dir: &Path,
    range: &str,
    limit: i64,
) -> AppResult<Vec<TopAlbumRow>> {
    let since = range_to_since_ms(range);

    let raw = sqlx::query_as::<_, TopAlbumRaw>(
        r#"
        SELECT al.id                          AS album_id,
               al.title                       AS title,
               al.artist_id                   AS artist_id,
               ar.name                        AS artist_name,
               COUNT(*)                       AS plays,
               COALESCE(SUM(pe.listened_ms), 0) AS listened_ms,
               aw.hash                        AS artwork_hash,
               aw.format                      AS artwork_format
          FROM play_event pe
          JOIN track t        ON t.id = pe.track_id
          JOIN album al       ON al.id = t.album_id
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (?1 IS NULL OR pe.played_at >= ?1)
         GROUP BY al.id
         ORDER BY plays DESC, listened_ms DESC
         LIMIT ?2
        "#,
    )
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(fmt)) => {
                        let full = artwork_dir
                            .join(format!("{hash}.{fmt}"))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            TopAlbumRow {
                album_id: row.album_id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                plays: row.plays,
                listened_ms: row.listened_ms,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
            }
        })
        .collect();

    Ok(rows)
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TopGenreRow {
    pub genre_id: i64,
    pub name: String,
    pub plays: i64,
    pub listened_ms: i64,
}

/// Top genres by listening time over `range`.
///
/// Joins `play_event` → `track_genre` → `genre`. A track can carry
/// several genres, so a single play credits every genre attached to
/// the track — that's intentional: a play of a "Rock; Indie" track
/// counts toward both buckets, matching how the user tagged it.
/// Ordered by `listened_ms` first (the user frames this as "hours of
/// rock vs jazz"), `plays` as the tiebreaker.
#[tauri::command]
pub async fn stats_top_genres(
    state: tauri::State<'_, AppState>,
    range: String,
    limit: i64,
) -> AppResult<Vec<TopGenreRow>> {
    let pool = state.require_profile_pool().await?;
    stats_top_genres_inner(&pool, &range, limit).await
}

async fn stats_top_genres_inner(
    pool: &SqlitePool,
    range: &str,
    limit: i64,
) -> AppResult<Vec<TopGenreRow>> {
    let since = range_to_since_ms(range);

    let rows = sqlx::query_as::<_, TopGenreRow>(
        r#"
        SELECT g.id                          AS genre_id,
               g.name                        AS name,
               COUNT(*)                      AS plays,
               COALESCE(SUM(pe.listened_ms), 0) AS listened_ms
          FROM play_event pe
          JOIN track_genre tg ON tg.track_id = pe.track_id
          JOIN genre g        ON g.id = tg.genre_id
         WHERE (?1 IS NULL OR pe.played_at >= ?1)
         GROUP BY g.id
         ORDER BY listened_ms DESC, plays DESC
         LIMIT ?2
        "#,
    )
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ListeningByDayRow {
    pub day: String,
    pub plays: i64,
    pub listened_ms: i64,
}

#[tauri::command]
pub async fn stats_listening_by_day(
    state: tauri::State<'_, AppState>,
    range: String,
) -> AppResult<Vec<ListeningByDayRow>> {
    let pool = state.require_profile_pool().await?;
    stats_listening_by_day_inner(&pool, &range).await
}

async fn stats_listening_by_day_inner(
    pool: &SqlitePool,
    range: &str,
) -> AppResult<Vec<ListeningByDayRow>> {
    let since = range_to_since_ms(range);

    let rows = sqlx::query_as::<_, ListeningByDayRow>(
        r#"
        SELECT strftime('%Y-%m-%d', played_at / 1000, 'unixepoch', 'localtime') AS day,
               COUNT(*)                                                        AS plays,
               COALESCE(SUM(listened_ms), 0)                                   AS listened_ms
          FROM play_event
         WHERE (?1 IS NULL OR played_at >= ?1)
         GROUP BY day
         ORDER BY day ASC
        "#,
    )
    .bind(since)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// 24-bucket histogram of plays by local hour-of-day, aggregated
/// across the whole range. Index `i` = hour `i` (0..=23). Always
/// returns exactly 24 entries, padding missing hours with zero so
/// the frontend can render without holes.
#[tauri::command]
pub async fn stats_listening_by_hour(
    state: tauri::State<'_, AppState>,
    range: String,
) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    stats_listening_by_hour_inner(&pool, &range).await
}

async fn stats_listening_by_hour_inner(pool: &SqlitePool, range: &str) -> AppResult<Vec<i64>> {
    let since = range_to_since_ms(range);

    let rows: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT strftime('%H', played_at / 1000, 'unixepoch', 'localtime') AS hour,
               COUNT(*)                                                   AS plays
          FROM play_event
         WHERE (?1 IS NULL OR played_at >= ?1)
         GROUP BY hour
        "#,
    )
    .bind(since)
    .fetch_all(pool)
    .await?;

    let mut buckets = vec![0_i64; 24];
    for (hour, plays) in rows {
        if let Ok(h) = hour.parse::<usize>() {
            if h < 24 {
                buckets[h] = plays;
            }
        }
    }
    Ok(buckets)
}

/// Bundle every stats aggregate for `range` into a single JSON string
/// so the user can save their listening history outside the app
/// (spreadsheets, scripts, archival). The frontend handles the actual
/// file-write via tauri-plugin-fs; the command only produces the blob.
///
/// Shape:
///
/// ```json
/// {
///   "schema_version": 2,
///   "exported_at": "...",   // RFC3339
///   "range": "30d",
///   "overview": { ... },
///   "top_tracks":  [ ... ],   // up to 100
///   "top_artists": [ ... ],
///   "top_albums":  [ ... ],
///   "top_genres":  [ ... ],
///   "listening_by_day":  [ ... ],
///   "listening_by_hour": [ ... ]  // 24 buckets
/// }
/// ```
///
/// `schema_version` bumps if we rename a field or reshape an object
/// so external tooling can refuse incompatible files.
///
/// Writes the JSON directly to `target_path` because shipping a
/// multi-MB blob through the IPC boundary just to hand it back to
/// disk is wasteful — and we don't depend on `tauri-plugin-fs` for
/// file writes anywhere else.
#[tauri::command]
pub async fn export_stats_json(
    state: tauri::State<'_, AppState>,
    range: String,
    target_path: String,
) -> AppResult<()> {
    // Resolve the profile pool + paths ONCE and thread the same
    // snapshot through every aggregate, instead of letting each
    // delegate re-resolve `require_profile_pool` independently. A
    // profile switch landing mid-export would otherwise mix two
    // profiles' data into one file. Limit `100` matches the upper
    // bound of the on-screen selectors so the export is "what you saw
    // plus a bit more".
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = &state.paths.metadata_artwork_dir;

    let overview = stats_overview_inner(&pool, &range).await?;
    let top_tracks = stats_top_tracks_inner(&pool, &artwork_dir, &range, 100).await?;
    let top_artists = stats_top_artists_inner(&pool, metadata_dir, &range, 100).await?;
    let top_albums = stats_top_albums_inner(&pool, &artwork_dir, &range, 100).await?;
    let top_genres = stats_top_genres_inner(&pool, &range, 100).await?;
    let listening_by_day = stats_listening_by_day_inner(&pool, &range).await?;
    let listening_by_hour = stats_listening_by_hour_inner(&pool, &range).await?;

    #[derive(Serialize)]
    struct Bundle<'a> {
        schema_version: u32,
        exported_at: String,
        range: &'a str,
        overview: &'a StatsOverview,
        top_tracks: &'a [TopTrackRow],
        top_artists: &'a [TopArtistRow],
        top_albums: &'a [TopAlbumRow],
        top_genres: &'a [TopGenreRow],
        listening_by_day: &'a [ListeningByDayRow],
        listening_by_hour: &'a [i64],
    }

    let bundle = Bundle {
        // v2 adds `top_genres`. Purely additive — existing readers
        // that ignore unknown keys stay compatible, but the bump lets
        // stricter tooling detect the new field.
        schema_version: 2,
        exported_at: chrono::Utc::now().to_rfc3339(),
        range: &range,
        overview: &overview,
        top_tracks: &top_tracks,
        top_artists: &top_artists,
        top_albums: &top_albums,
        top_genres: &top_genres,
        listening_by_day: &listening_by_day,
        listening_by_hour: &listening_by_hour,
    };

    let json = serde_json::to_string_pretty(&bundle)
        .map_err(|e| crate::error::AppError::Other(format!("stats serialize: {e}")))?;

    // Write via `spawn_blocking` so the tokio runtime isn't held
    // hostage on a slow disk. The payload is tiny relative to a
    // profile export so we don't bother streaming.
    let target = std::path::PathBuf::from(target_path);
    tokio::task::spawn_blocking(move || std::fs::write(&target, json))
        .await
        .map_err(|e| crate::error::AppError::Other(format!("stats write task: {e}")))?
        .map_err(|e| crate::error::AppError::Other(format!("stats file write: {e}")))?;
    Ok(())
}
