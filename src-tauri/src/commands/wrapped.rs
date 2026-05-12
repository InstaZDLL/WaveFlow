//! "WaveFlow Wrapped" — a Spotify-style year-in-review built entirely
//! from local `play_event` rows. Returns one bundle per year covering
//! the overview, tops, mood profile (avg BPM + LUFS), monthly
//! distribution, first listen of the year, most active day, and
//! longest consecutive-day listening streak.
//!
//! Year bounds are computed in local time (Jan 1 00:00 → Dec 31
//! 23:59:59.999) so a play at 11:59 PM on Dec 31 lands in the right
//! year regardless of UTC offset. Each sub-query reuses the same
//! `since_ms..until_ms` half-open window.

use chrono::{Datelike, Local, TimeZone};
use serde::Serialize;
use sqlx::FromRow;

use crate::{
    commands::stats::{TopAlbumRow, TopArtistRow, TopTrackRow},
    error::AppResult,
    state::AppState,
};

#[derive(Debug, Clone, Serialize)]
pub struct WrappedPayload {
    pub year: i32,
    pub total_plays: i64,
    pub total_listened_ms: i64,
    pub unique_tracks: i64,
    pub unique_artists: i64,
    pub unique_albums: i64,
    pub top_tracks: Vec<TopTrackRow>,
    pub top_artists: Vec<TopArtistRow>,
    pub top_albums: Vec<TopAlbumRow>,
    pub by_month: [MonthBucket; 12],
    pub by_hour: [i64; 24],
    pub most_active_day: Option<ActiveDay>,
    pub mood: MoodProfile,
    pub first_listen: Option<FirstListen>,
    pub streak: Option<Streak>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct MonthBucket {
    pub plays: i64,
    pub listened_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveDay {
    pub day: String, // "YYYY-MM-DD"
    pub plays: i64,
    pub listened_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MoodProfile {
    /// Listening-weighted average BPM (NULL if no analysed track was
    /// played that year). Weight is `listened_ms`, so a single 4-min
    /// play of a fast track counts more than a 15 s skip of a slow one.
    pub avg_bpm: Option<f64>,
    /// Listening-weighted average integrated loudness (LUFS).
    pub avg_lufs: Option<f64>,
    /// Coarse label derived from `avg_bpm`. Localised on the frontend
    /// via a fixed mapping so we don't ship copy from the backend.
    pub energy: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FirstListen {
    pub track_id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub played_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Streak {
    pub days: i64,
    pub start: String,
    pub end: String,
}

/// Local-time year bounds in epoch ms. The lower bound is inclusive,
/// the upper bound exclusive (`< next year`) so we don't fight DST or
/// leap-second drift around the year boundary.
fn year_bounds_ms(year: i32) -> (i64, i64) {
    let start = Local
        .with_ymd_and_hms(year, 1, 1, 0, 0, 0)
        .single()
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0);
    let end = Local
        .with_ymd_and_hms(year + 1, 1, 1, 0, 0, 0)
        .single()
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(i64::MAX);
    (start, end)
}

fn energy_label_from_bpm(bpm: f64) -> &'static str {
    match bpm {
        b if b < 80.0 => "chill",
        b if b < 110.0 => "warm",
        b if b < 135.0 => "groove",
        b if b < 160.0 => "energetic",
        _ => "fire",
    }
}

/// Years for which the active profile has at least one `play_event`.
/// Sorted descending so the UI shows the most recent year first.
#[tauri::command]
pub async fn available_wrapped_years(state: tauri::State<'_, AppState>) -> AppResult<Vec<i32>> {
    let pool = state.require_profile_pool().await?;
    let rows: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT DISTINCT strftime('%Y', played_at / 1000, 'unixepoch', 'localtime') AS y
          FROM play_event
         ORDER BY y DESC
        "#,
    )
    .fetch_all(&pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(s,)| s.parse::<i32>().ok())
        .collect())
}

#[tauri::command]
pub async fn get_wrapped(
    state: tauri::State<'_, AppState>,
    year: i32,
) -> AppResult<WrappedPayload> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = &state.paths.metadata_artwork_dir;
    let (since, until) = year_bounds_ms(year);

    // ---- Overview (plays / ms / unique tracks / albums) ----
    let (total_plays, total_ms, unique_tracks, unique_albums): (i64, i64, i64, i64) =
        sqlx::query_as(
            r#"
            SELECT COUNT(*),
                   COALESCE(SUM(pe.listened_ms), 0),
                   COUNT(DISTINCT pe.track_id),
                   COUNT(DISTINCT t.album_id)
              FROM play_event pe
              JOIN track t ON t.id = pe.track_id
             WHERE pe.played_at >= ?1 AND pe.played_at < ?2
            "#,
        )
        .bind(since)
        .bind(until)
        .fetch_one(&pool)
        .await?;

    let unique_artists: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(DISTINCT ta.artist_id)
          FROM play_event pe
          JOIN track_artist ta ON ta.track_id = pe.track_id
         WHERE pe.played_at >= ?1 AND pe.played_at < ?2
        "#,
    )
    .bind(since)
    .bind(until)
    .fetch_one(&pool)
    .await
    .unwrap_or(0);

    // ---- Tops — reuse the same row shapes as the stats commands so
    //      the frontend's existing artwork resolver works unchanged. ----
    let top_tracks = top_tracks_for_window(&pool, &artwork_dir, since, until, 10).await?;
    let top_artists = top_artists_for_window(&pool, metadata_dir, since, until, 10).await?;
    let top_albums = top_albums_for_window(&pool, &artwork_dir, since, until, 5).await?;

    // ---- Monthly histogram (12 buckets, calendar order, padded) ----
    let month_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        r#"
        SELECT strftime('%m', played_at / 1000, 'unixepoch', 'localtime') AS m,
               COUNT(*)                                                   AS plays,
               COALESCE(SUM(listened_ms), 0)                              AS listened_ms
          FROM play_event
         WHERE played_at >= ?1 AND played_at < ?2
         GROUP BY m
        "#,
    )
    .bind(since)
    .bind(until)
    .fetch_all(&pool)
    .await?;
    let mut by_month: [MonthBucket; 12] = Default::default();
    for (m, plays, ms) in month_rows {
        if let Ok(idx) = m.parse::<usize>() {
            if (1..=12).contains(&idx) {
                by_month[idx - 1] = MonthBucket {
                    plays,
                    listened_ms: ms,
                };
            }
        }
    }

    // ---- Hour histogram (24 buckets) ----
    let hour_rows: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT strftime('%H', played_at / 1000, 'unixepoch', 'localtime') AS h,
               COUNT(*)                                                   AS plays
          FROM play_event
         WHERE played_at >= ?1 AND played_at < ?2
         GROUP BY h
        "#,
    )
    .bind(since)
    .bind(until)
    .fetch_all(&pool)
    .await?;
    let mut by_hour = [0_i64; 24];
    for (h, plays) in hour_rows {
        if let Ok(idx) = h.parse::<usize>() {
            if idx < 24 {
                by_hour[idx] = plays;
            }
        }
    }

    // ---- Most active day (by total listened_ms, ties broken by plays) ----
    let most_active_day: Option<ActiveDay> = sqlx::query_as::<_, (String, i64, i64)>(
        r#"
        SELECT strftime('%Y-%m-%d', played_at / 1000, 'unixepoch', 'localtime') AS day,
               COUNT(*)                                                          AS plays,
               COALESCE(SUM(listened_ms), 0)                                     AS listened_ms
          FROM play_event
         WHERE played_at >= ?1 AND played_at < ?2
         GROUP BY day
         ORDER BY listened_ms DESC, plays DESC
         LIMIT 1
        "#,
    )
    .bind(since)
    .bind(until)
    .fetch_optional(&pool)
    .await?
    .map(|(day, plays, ms)| ActiveDay {
        day,
        plays,
        listened_ms: ms,
    });

    // ---- Mood profile (listening-weighted BPM + LUFS) ----
    //
    // We weight by `listened_ms` so a 4 min play of a fast track
    // counts ~16× a 15 s skip of a slow one — otherwise a hate-skip
    // collection would skew the average.
    #[derive(FromRow)]
    struct MoodRaw {
        weighted_bpm: Option<f64>,
        weighted_lufs: Option<f64>,
        total_weight: Option<i64>,
    }
    let mood_raw: MoodRaw = sqlx::query_as(
        r#"
        SELECT SUM(ta.bpm           * pe.listened_ms) AS weighted_bpm,
               SUM(ta.loudness_lufs * pe.listened_ms) AS weighted_lufs,
               SUM(CASE WHEN ta.bpm IS NOT NULL OR ta.loudness_lufs IS NOT NULL
                        THEN pe.listened_ms ELSE 0 END) AS total_weight
          FROM play_event pe
          JOIN track_analysis ta ON ta.track_id = pe.track_id
         WHERE pe.played_at >= ?1 AND pe.played_at < ?2
        "#,
    )
    .bind(since)
    .bind(until)
    .fetch_one(&pool)
    .await
    .unwrap_or(MoodRaw {
        weighted_bpm: None,
        weighted_lufs: None,
        total_weight: None,
    });
    let weight = mood_raw.total_weight.unwrap_or(0);
    let (avg_bpm, avg_lufs) = if weight > 0 {
        (
            mood_raw.weighted_bpm.map(|v| v / weight as f64),
            mood_raw.weighted_lufs.map(|v| v / weight as f64),
        )
    } else {
        (None, None)
    };
    let mood = MoodProfile {
        avg_bpm,
        avg_lufs,
        energy: avg_bpm.map(|b| energy_label_from_bpm(b).to_string()),
    };

    // ---- First listen of the year ----
    let first_listen: Option<FirstListen> = sqlx::query_as::<_, (i64, String, Option<String>, i64)>(
        r#"
        SELECT pe.track_id,
               t.title,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               pe.played_at
          FROM play_event pe
          JOIN track t ON t.id = pe.track_id
         WHERE pe.played_at >= ?1 AND pe.played_at < ?2
         ORDER BY pe.played_at ASC
         LIMIT 1
        "#,
    )
    .bind(since)
    .bind(until)
    .fetch_optional(&pool)
    .await?
    .map(|(track_id, title, artist_name, played_at)| FirstListen {
        track_id,
        title,
        artist_name,
        played_at,
    });

    // ---- Longest consecutive-day listening streak ----
    //
    // We list every distinct local-date that has at least one play,
    // then walk the sorted list and count the longest run of dates
    // that increment by exactly one day. The set is bounded by 366
    // rows per year — no need for a fancier SQL gaps-and-islands.
    let day_strs: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT DISTINCT strftime('%Y-%m-%d', played_at / 1000, 'unixepoch', 'localtime') AS day
          FROM play_event
         WHERE played_at >= ?1 AND played_at < ?2
         ORDER BY day ASC
        "#,
    )
    .bind(since)
    .bind(until)
    .fetch_all(&pool)
    .await?;
    let streak = compute_longest_streak(day_strs.into_iter().map(|(s,)| s));

    Ok(WrappedPayload {
        year,
        total_plays,
        total_listened_ms: total_ms,
        unique_tracks,
        unique_artists,
        unique_albums,
        top_tracks,
        top_artists,
        top_albums,
        by_month,
        by_hour,
        most_active_day,
        mood,
        first_listen,
        streak,
    })
}

fn compute_longest_streak(days: impl Iterator<Item = String>) -> Option<Streak> {
    use chrono::NaiveDate;

    let mut best: Option<(NaiveDate, NaiveDate, i64)> = None; // (start, end, length)
    let mut cur_start: Option<NaiveDate> = None;
    let mut cur_end: Option<NaiveDate> = None;
    let mut cur_len: i64 = 0;

    for day in days {
        let Ok(d) = NaiveDate::parse_from_str(&day, "%Y-%m-%d") else {
            continue;
        };
        match cur_end {
            Some(prev) if d == prev.succ_opt().unwrap_or(prev) => {
                cur_end = Some(d);
                cur_len += 1;
            }
            _ => {
                cur_start = Some(d);
                cur_end = Some(d);
                cur_len = 1;
            }
        }
        // Update best at each step so the very last run is captured
        // without a trailing flush.
        let (Some(s), Some(e)) = (cur_start, cur_end) else {
            continue;
        };
        match best {
            Some((_, _, n)) if n >= cur_len => {}
            _ => best = Some((s, e, cur_len)),
        }
    }
    best.map(|(s, e, n)| Streak {
        days: n,
        start: s.format("%Y-%m-%d").to_string(),
        end: e.format("%Y-%m-%d").to_string(),
    })
}

// ===== helpers — bounded-window versions of the public stats commands.
// =====
//
// We could have made the existing stats commands accept arbitrary
// bounds, but the public API was already shipped and adding a half-open
// window everywhere would balloon the call sites. The duplication here
// is < 80 lines and stays local to Wrapped.

async fn top_tracks_for_window(
    pool: &sqlx::SqlitePool,
    artwork_dir: &std::path::Path,
    since: i64,
    until: i64,
    limit: i64,
) -> AppResult<Vec<TopTrackRow>> {
    #[derive(FromRow)]
    struct Raw {
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
    let rows = sqlx::query_as::<_, Raw>(
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
         WHERE pe.played_at >= ?1 AND pe.played_at < ?2
         GROUP BY t.id
         ORDER BY plays DESC, listened_ms DESC
         LIMIT ?3
        "#,
    )
    .bind(since)
    .bind(until)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
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
        .collect())
}

async fn top_artists_for_window(
    pool: &sqlx::SqlitePool,
    metadata_dir: &std::path::Path,
    since: i64,
    until: i64,
    limit: i64,
) -> AppResult<Vec<TopArtistRow>> {
    #[derive(FromRow)]
    struct Raw {
        artist_id: i64,
        name: String,
        plays: i64,
        listened_ms: i64,
        picture_url: Option<String>,
        picture_hash: Option<String>,
    }
    let rows = sqlx::query_as::<_, Raw>(
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
         WHERE pe.played_at >= ?1 AND pe.played_at < ?2
         GROUP BY ar.id
         ORDER BY plays DESC, listened_ms DESC
         LIMIT ?3
        "#,
    )
    .bind(since)
    .bind(until)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
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
        .collect())
}

async fn top_albums_for_window(
    pool: &sqlx::SqlitePool,
    artwork_dir: &std::path::Path,
    since: i64,
    until: i64,
    limit: i64,
) -> AppResult<Vec<TopAlbumRow>> {
    #[derive(FromRow)]
    struct Raw {
        album_id: i64,
        title: String,
        artist_id: Option<i64>,
        artist_name: Option<String>,
        plays: i64,
        listened_ms: i64,
        artwork_hash: Option<String>,
        artwork_format: Option<String>,
    }
    let rows = sqlx::query_as::<_, Raw>(
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
         WHERE pe.played_at >= ?1 AND pe.played_at < ?2
         GROUP BY al.id
         ORDER BY plays DESC, listened_ms DESC
         LIMIT ?3
        "#,
    )
    .bind(since)
    .bind(until)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
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
        .collect())
}

/// Make the current year available as a sensible default for the
/// frontend year picker.
#[tauri::command]
pub fn wrapped_current_year() -> i32 {
    Local::now().year()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streak_walks_consecutive_days() {
        let days = [
            "2026-01-01",
            "2026-01-02",
            "2026-01-03",
            "2026-01-05", // gap
            "2026-01-06",
            "2026-01-07",
            "2026-01-08",
            "2026-01-09",
        ];
        let streak = compute_longest_streak(days.iter().map(|s| s.to_string())).unwrap();
        assert_eq!(streak.days, 5);
        assert_eq!(streak.start, "2026-01-05");
        assert_eq!(streak.end, "2026-01-09");
    }

    #[test]
    fn streak_handles_single_day() {
        let s = compute_longest_streak(["2026-03-01".to_string()].into_iter()).unwrap();
        assert_eq!(s.days, 1);
        assert_eq!(s.start, "2026-03-01");
        assert_eq!(s.end, "2026-03-01");
    }

    #[test]
    fn streak_handles_empty() {
        assert!(compute_longest_streak(std::iter::empty()).is_none());
    }

    #[test]
    fn energy_label_buckets() {
        assert_eq!(energy_label_from_bpm(70.0), "chill");
        assert_eq!(energy_label_from_bpm(100.0), "warm");
        assert_eq!(energy_label_from_bpm(125.0), "groove");
        assert_eq!(energy_label_from_bpm(150.0), "energetic");
        assert_eq!(energy_label_from_bpm(170.0), "fire");
    }
}
