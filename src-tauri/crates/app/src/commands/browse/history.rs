//! The profile's counters, its recent plays and the play history.

use serde::Serialize;
use sqlx::FromRow;

use crate::{error::AppResult, state::AppState};

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
        .fetch_one(&*pool)
        .await
        .unwrap_or(0);

    let recent_plays_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT track_id) FROM play_event
          WHERE completed = 1 OR listened_ms >= 15000",
    )
    .fetch_one(&*pool)
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

/// List the most-recently-played tracks for a library, deduplicated
/// to one entry per track (taking the max `played_at` across all
/// `play_event` rows for that track). How many rows come back is the
/// caller's `limit`: the home carousel asks for 12, the playlist
/// context menu for 300.
///
/// Not what the sidebar's "Récemment joués" entry renders — that opens
/// `HistoryView`, which goes through `list_play_history` and keeps the
/// repeats this command folds away.
#[tauri::command]
pub async fn list_recent_plays(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    limit: i64,
) -> AppResult<Vec<RecentPlay>> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

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
    .fetch_all(&*pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
                row.artwork_hash.as_deref(),
                row.artwork_format.as_deref(),
                Some(&artwork_dir),
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
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

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
    .fetch_all(&*pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
                row.artwork_hash.as_deref(),
                row.artwork_format.as_deref(),
                Some(&artwork_dir),
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
    .fetch_all(&*pool)
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
