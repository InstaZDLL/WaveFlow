//! Mood-based radio — Spotify-style "moment of the day" queues.
//!
//! Five presets, each mapped to a BPM range plus an optional LUFS
//! ceiling (focus/sleep want quieter tracks; workout/party are tempo-
//! only). The query joins `track` with `track_analysis` and filters
//! out tracks that have no analysis row at all — without BPM data we
//! can't honour the constraint, and a "mood" radio that randomly
//! sneaks in heavy metal between two ambient tracks would defeat the
//! whole point.
//!
//! Returns the ordered `Vec<i64>` of track IDs. The frontend hands
//! this to `player_play_tracks` with `source_type = "radio"` so
//! play_event rows still get tagged for stats — the existing CHECK
//! constraint on `queue_item.source_type` doesn't allow a `'mood'`
//! variant and adding one would mean a migration just for analytics
//! granularity, which isn't worth the churn yet.

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Target queue size. Same logic as the seed-based radio: long enough
/// to forget about it, short enough that the tail stays on-vibe.
const TARGET_LEN: usize = 40;

/// Hard cap on tracks per primary artist. Without this the mood radio
/// would collapse to "your top artist on repeat" once a heavy listener
/// has 100+ play_events on a single act.
const PER_ARTIST_CAP: usize = 4;

/// Pool size before the per-artist cap + shuffle. Larger = more
/// variety in the candidate set, but also slower SQL — 400 hits the
/// sweet spot for libraries up to ~50k tracks.
const POOL_SIZE: i64 = 400;

use waveflow_core::album_playback::{ALBUM_FIT, ALBUM_MIN_ANALYSED};

/// Album mode: albums pulled before budgeting. Smaller than
/// [`POOL_SIZE`] because each row is a whole record rather than one
/// track.
const ALBUM_POOL_SIZE: i64 = 60;

/// Album mode: records per primary artist. Lower than
/// [`PER_ARTIST_CAP`] because a record is already several tracks of
/// the same artist in a row.
const ALBUM_PER_ARTIST_CAP: usize = 2;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mood {
    Focus,
    Chill,
    Workout,
    Party,
    Sleep,
}

struct MoodFilter {
    /// Inclusive BPM range. `None` means unbounded on that side.
    bpm_min: Option<f64>,
    bpm_max: Option<f64>,
    /// Inclusive LUFS ceiling — `Some(-14.0)` keeps tracks at or below
    /// −14 LUFS (perceptually quieter, fits Focus / Sleep). LUFS is
    /// negative; tracks with NULL loudness pass through (the constraint
    /// only filters when we have data, so we don't punish unanalysed
    /// quiet tracks).
    lufs_max: Option<f64>,
}

impl Mood {
    fn filter(&self) -> MoodFilter {
        match self {
            // Calm, mid-tempo, quiet — picked by ear after a few work
            // sessions. The −14 LUFS ceiling is roughly Spotify's
            // normalisation target; tracks louder than that tend to
            // pull attention away from whatever you're focusing on.
            Mood::Focus => MoodFilter {
                bpm_min: Some(60.0),
                bpm_max: Some(110.0),
                lufs_max: Some(-14.0),
            },
            // Lounge / chill territory — same tempo band as Focus but
            // no loudness constraint so the radio can include warmer
            // mixes (jazz, soul, downtempo).
            Mood::Chill => MoodFilter {
                bpm_min: Some(60.0),
                bpm_max: Some(100.0),
                lufs_max: None,
            },
            // Workout: keeps the cadence above ~125 (running, lifting).
            // Upper bound at 180 to avoid drum'n'bass / speedcore that
            // most users wouldn't want for a treadmill session.
            Mood::Workout => MoodFilter {
                bpm_min: Some(125.0),
                bpm_max: Some(180.0),
                lufs_max: None,
            },
            // Dance-pop tempo band, broad on purpose — house (~120),
            // pop (~120-130), reggaeton (~95-100, deliberately
            // excluded so a "Party" radio doesn't accidentally drop
            // tempo mid-set).
            Mood::Party => MoodFilter {
                bpm_min: Some(110.0),
                bpm_max: Some(140.0),
                lufs_max: None,
            },
            // Sleep: very slow, very quiet. The −18 LUFS floor catches
            // almost anything that isn't ambient / piano / classical.
            Mood::Sleep => MoodFilter {
                bpm_min: None,
                bpm_max: Some(75.0),
                lufs_max: Some(-18.0),
            },
        }
    }
}

#[tauri::command]
pub async fn start_mood_radio(
    state: tauri::State<'_, AppState>,
    mood: Mood,
) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    let f = mood.filter();

    if waveflow_core::album_playback::album_mode_enabled(&pool).await {
        // Album mode is a preference, not a contract: a library whose
        // records are mostly unanalysed can satisfy the mood track by
        // track while no whole record qualifies. Falling through then
        // plays music instead of returning an error — and if there is
        // genuinely nothing, the track path says so in the words that
        // actually apply ("no tracks match this mood") rather than
        // blaming the albums.
        let by_album = mood_radio_by_album(&pool, &f).await?;
        if !by_album.is_empty() {
            return Ok(by_album);
        }
        tracing::info!("no record fits this mood; falling back to individual tracks");
    }

    // Pull the candidate pool. `bpm IS NOT NULL` is mandatory — we
    // can't honour a tempo-based mood without it. LUFS is optional
    // (NULL passes through when no ceiling is set, otherwise the
    // ceiling acts as a soft "skip very loud tracks" filter).
    let rows: Vec<TrackCandidate> = sqlx::query_as::<_, TrackCandidate>(
        r#"
        SELECT t.id              AS track_id,
               t.primary_artist  AS primary_artist,
               COALESCE(ta.bpm, 0.0)             AS bpm,
               COALESCE(ta.loudness_lufs, 0.0)   AS lufs
          FROM track t
          JOIN track_analysis ta ON ta.track_id = t.id
         WHERE t.is_available = 1
           AND ta.bpm IS NOT NULL
           AND (?1 IS NULL OR ta.bpm >= ?1)
           AND (?2 IS NULL OR ta.bpm <= ?2)
           AND (?3 IS NULL OR ta.loudness_lufs IS NULL OR ta.loudness_lufs <= ?3)
         ORDER BY RANDOM()
         LIMIT ?4
        "#,
    )
    .bind(f.bpm_min)
    .bind(f.bpm_max)
    .bind(f.lufs_max)
    .bind(POOL_SIZE)
    .fetch_all(&*pool)
    .await?;

    if rows.is_empty() {
        return Err(AppError::Other(
            "no tracks match this mood — run BPM analysis on your library first".into(),
        ));
    }

    // Per-artist cap so a single heavy contributor doesn't dominate.
    let mut per_artist: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut out: Vec<i64> = Vec::with_capacity(TARGET_LEN);
    for c in rows {
        if out.len() >= TARGET_LEN {
            break;
        }
        let count = per_artist.entry(c.primary_artist).or_insert(0);
        if *count >= PER_ARTIST_CAP {
            continue;
        }
        *count += 1;
        out.push(c.track_id);
    }

    Ok(out)
}

/// The same radio, built out of whole records (#618).
///
/// Selection is per album rather than per track: a record qualifies
/// when most of what we have measured of it sits inside the mood's
/// window, and it then plays in the order it was pressed. The
/// per-artist cap becomes a cap on records, for the same reason —
/// without it a heavy listener's mood radio is one artist's
/// discography.
async fn mood_radio_by_album(pool: &SqlitePool, f: &MoodFilter) -> AppResult<Vec<i64>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        album_id: i64,
        /// Every playable track on the record, analysed or not — this
        /// is the budgeting unit, and it has to match what actually
        /// gets queued.
        track_count: i64,
        /// `track.primary_artist` is nullable (`ON DELETE SET NULL`),
        /// so an album whose tracks have all lost theirs aggregates to
        /// NULL. Decoding that into an `i64` fails the whole query,
        /// which would take the radio down with it.
        primary_artist: Option<i64>,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT t.album_id       AS album_id,
               COUNT(*)         AS track_count,
               MIN(t.primary_artist) AS primary_artist
          FROM track t
          LEFT JOIN track_analysis ta ON ta.track_id = t.id
         WHERE t.is_available = 1
           AND t.album_id IS NOT NULL
         GROUP BY t.album_id
        HAVING SUM(CASE WHEN ta.bpm IS NOT NULL THEN 1 ELSE 0 END) >= ?5
           AND SUM(CASE WHEN ta.bpm IS NOT NULL
                         AND (?1 IS NULL OR ta.bpm >= ?1)
                         AND (?2 IS NULL OR ta.bpm <= ?2)
                         AND (?3 IS NULL OR ta.loudness_lufs IS NULL
                              OR ta.loudness_lufs <= ?3)
                        THEN 1 ELSE 0 END) * 1.0
               / SUM(CASE WHEN ta.bpm IS NOT NULL THEN 1 ELSE 0 END) >= ?4
         ORDER BY RANDOM()
         LIMIT ?6
        "#,
    )
    .bind(f.bpm_min)
    .bind(f.bpm_max)
    .bind(f.lufs_max)
    .bind(ALBUM_FIT)
    .bind(ALBUM_MIN_ANALYSED)
    .bind(ALBUM_POOL_SIZE)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        // Empty rather than an error: the caller decides what an empty
        // album selection means, and it means "try tracks".
        return Ok(vec![]);
    }

    let mut per_artist: std::collections::HashMap<Option<i64>, usize> =
        std::collections::HashMap::new();
    let candidates: Vec<waveflow_core::album_playback::AlbumCandidate> = rows
        .into_iter()
        .filter(|row| {
            // Records with no artist at all share one bucket rather
            // than each counting as a different artist: that is the
            // conservative reading, and it keeps a pile of untagged
            // rips from filling the radio between them.
            let count = per_artist.entry(row.primary_artist).or_insert(0);
            if *count >= ALBUM_PER_ARTIST_CAP {
                return false;
            }
            *count += 1;
            true
        })
        .map(|row| waveflow_core::album_playback::AlbumCandidate {
            album_id: row.album_id,
            track_count: row.track_count,
        })
        .collect();

    let chosen = waveflow_core::album_playback::fit_albums_to_budget(&candidates, TARGET_LEN);
    Ok(waveflow_core::album_playback::tracks_in_album_order(pool, &chosen).await?)
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct TrackCandidate {
    track_id: i64,
    primary_artist: i64,
    #[allow(dead_code)]
    bpm: f64,
    #[allow(dead_code)]
    lufs: f64,
}

/// Returns how many library tracks would qualify for each mood, so the
/// frontend can disable buttons that would yield empty radios (typical
/// for libraries where BPM analysis hasn't been run yet, or where the
/// user has no slow tracks at all).
#[tauri::command]
pub async fn mood_radio_counts(state: tauri::State<'_, AppState>) -> AppResult<MoodCounts> {
    let pool = state.require_profile_pool().await?;
    Ok(MoodCounts {
        focus: count_for_mood(&pool, &Mood::Focus.filter()).await?,
        chill: count_for_mood(&pool, &Mood::Chill.filter()).await?,
        workout: count_for_mood(&pool, &Mood::Workout.filter()).await?,
        party: count_for_mood(&pool, &Mood::Party.filter()).await?,
        sleep: count_for_mood(&pool, &Mood::Sleep.filter()).await?,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct MoodCounts {
    pub focus: i64,
    pub chill: i64,
    pub workout: i64,
    pub party: i64,
    pub sleep: i64,
}

async fn count_for_mood(pool: &SqlitePool, f: &MoodFilter) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
          FROM track t
          JOIN track_analysis ta ON ta.track_id = t.id
         WHERE t.is_available = 1
           AND ta.bpm IS NOT NULL
           AND (?1 IS NULL OR ta.bpm >= ?1)
           AND (?2 IS NULL OR ta.bpm <= ?2)
           AND (?3 IS NULL OR ta.loudness_lufs IS NULL OR ta.loudness_lufs <= ?3)
        "#,
    )
    .bind(f.bpm_min)
    .bind(f.bpm_max)
    .bind(f.lufs_max)
    .fetch_one(pool)
    .await?;
    Ok(n)
}
