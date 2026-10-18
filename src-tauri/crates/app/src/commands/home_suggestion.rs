//! Home's suggestion card: one track, and why it was picked.
//!
//! The kinds, the rotation and the draw are in
//! [`waveflow_core::home_suggestion`]; this module only asks the profile
//! database for each kind's shortlist. Everything is local: the similar
//! artists come from the cache the artist pages and the radio fill, and
//! nothing here goes to the network.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use waveflow_core::{
    home_suggestion::{
        kind_order, pick_index, shortlist, vibe_distance, RecentProfile, SuggestionKind, DAY_MS,
        FORGOTTEN_IDLE_DAYS, FORGOTTEN_MIN_PLAYS, QUALIFIED_PLAY_MS, SHORTLIST_LEN,
        SHORTLIST_PER_ARTIST, VIBE_FRESH_DAYS, VIBE_LOOKBACK_DAYS, VIBE_TOP_ARTISTS,
    },
    repository::{sqlite::SqliteTrackRepository, track::TrackRepository},
    similar::cached_similar_library_ids,
};

use crate::{
    commands::track::{track_from_row, Track},
    error::AppResult,
    state::AppState,
};

/// Candidates read per kind before ranking and capping.
const POOL_LIMIT: i64 = 400;
/// The vibe pool is ranked here rather than in SQL, so it reads wider:
/// cut by id, it would favour the oldest tracks of the artists it holds.
const VIBE_POOL_LIMIT: i64 = 5_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeSuggestion {
    pub kind: SuggestionKind,
    pub track: Track,
    /// Forgotten: qualified plays, all time.
    pub plays: Option<i64>,
    /// Forgotten: the last play of any length, in ms.
    pub last_played_at: Option<i64>,
    /// Vibe: the recently played artist the pick comes from.
    pub anchor_artist: Option<String>,
    /// Unplayed: qualified plays of the track's artist, all time.
    pub artist_plays: Option<i64>,
    /// Unplayed: when the track was added, in ms.
    pub added_at: Option<i64>,
}

/// A suggestion already shown for this day and click count, kept by the
/// frontend (`home.suggestion_draw`) and handed back so the same track
/// comes back: once played, it no longer qualifies for its kind, and a
/// fresh draw would replace it the next time Home opens.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedSuggestion {
    kind: SuggestionKind,
    track_id: i64,
    plays: Option<i64>,
    last_played_at: Option<i64>,
    anchor_artist: Option<String>,
    artist_plays: Option<i64>,
    added_at: Option<i64>,
}

/// What the shortlist query of a kind found for one track.
#[derive(Debug, Clone, Default)]
struct Candidate {
    track_id: i64,
    artist_id: Option<i64>,
    plays: Option<i64>,
    last_played_at: Option<i64>,
    anchor_artist: Option<String>,
    artist_plays: Option<i64>,
    added_at: Option<i64>,
}

/// Home's suggestion for `day` (days since the epoch in the user's time
/// zone) after `attempt` clicks on "Another suggestion". `None` when no
/// kind has anything to offer, an empty library for one.
///
/// `pinned` is the suggestion this day and click count already gave: it
/// is returned again, with its reason as first drawn, as long as its
/// track is still available. Only otherwise is a new draw made.
#[tauri::command]
pub async fn get_home_suggestion(
    state: tauri::State<'_, AppState>,
    day: i64,
    attempt: u32,
    pinned: Option<PinnedSuggestion>,
) -> AppResult<Option<HomeSuggestion>> {
    let (pool, profile_id) = state.require_profile_pool_with_id(None).await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let repo = SqliteTrackRepository::new((*pool).clone());

    if let Some(pin) = pinned {
        let available: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM track WHERE id = ? AND is_available = 1")
                .bind(pin.track_id)
                .fetch_optional(&*pool)
                .await?;
        if available.is_some() {
            if let Some(row) = repo.get(pin.track_id).await? {
                return Ok(Some(HomeSuggestion {
                    kind: pin.kind,
                    track: track_from_row(row, &artwork_dir),
                    plays: pin.plays,
                    last_played_at: pin.last_played_at,
                    anchor_artist: pin.anchor_artist,
                    artist_plays: pin.artist_plays,
                    added_at: pin.added_at,
                }));
            }
        }
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    for kind in kind_order(day, attempt) {
        let candidates = match kind {
            SuggestionKind::Forgotten => forgotten(&pool, now_ms).await?,
            SuggestionKind::Vibe => vibe(&pool, now_ms).await?,
            SuggestionKind::Unplayed => unplayed(&pool).await?,
        };
        if candidates.is_empty() {
            continue;
        }
        let pick = candidates[pick_index(candidates.len(), day, attempt, kind)].clone();
        let Some(row) = repo.get(pick.track_id).await? else {
            continue;
        };
        let track = track_from_row(row, &artwork_dir);
        return Ok(Some(HomeSuggestion {
            kind,
            track,
            plays: pick.plays,
            last_played_at: pick.last_played_at,
            anchor_artist: pick.anchor_artist,
            artist_plays: pick.artist_plays,
            added_at: pick.added_at,
        }));
    }
    Ok(None)
}

/// Played through at least [`FORGOTTEN_MIN_PLAYS`] times, and not at all
/// for [`FORGOTTEN_IDLE_DAYS`]; the most played first. Only qualified plays
/// are counted, but any play, a skip included, makes the track recent: one
/// skipped last week has not been forgotten.
async fn forgotten(pool: &SqlitePool, now_ms: i64) -> AppResult<Vec<Candidate>> {
    let rows: Vec<(i64, Option<i64>, i64, i64)> = sqlx::query_as(
        r#"
        SELECT pe.track_id, t.primary_artist,
               SUM(CASE WHEN pe.completed = 1 OR pe.listened_ms >= ?1 THEN 1 ELSE 0 END)
                   AS plays,
               MAX(pe.played_at) AS last_played
          FROM play_event pe
          JOIN track t ON t.id = pe.track_id
         WHERE t.is_available = 1
         GROUP BY pe.track_id
        HAVING plays >= ?2 AND last_played < ?3
         ORDER BY plays DESC, last_played ASC, pe.track_id
         LIMIT ?4
        "#,
    )
    .bind(QUALIFIED_PLAY_MS)
    .bind(FORGOTTEN_MIN_PLAYS)
    .bind(now_ms - FORGOTTEN_IDLE_DAYS * DAY_MS)
    .bind(POOL_LIMIT)
    .fetch_all(pool)
    .await?;
    let ranked = rows
        .into_iter()
        .map(|(track_id, artist_id, plays, last)| Candidate {
            track_id,
            artist_id,
            plays: Some(plays),
            last_played_at: Some(last),
            ..Candidate::default()
        });
    Ok(shortlist(
        ranked,
        |c| c.artist_id,
        SHORTLIST_PER_ARTIST,
        SHORTLIST_LEN,
    ))
}

/// In line with the last [`VIBE_LOOKBACK_DAYS`]: tracks by the artists
/// played most then, or by artists similar to them, not played for
/// [`VIBE_FRESH_DAYS`], closest to that listening's energy and tempo
/// first.
async fn vibe(pool: &SqlitePool, now_ms: i64) -> AppResult<Vec<Candidate>> {
    let since = now_ms - VIBE_LOOKBACK_DAYS * DAY_MS;
    let top: Vec<(i64, String)> = sqlx::query_as(
        r#"
        SELECT t.primary_artist, a.name
          FROM play_event pe
          JOIN track t ON t.id = pe.track_id
          JOIN artist a ON a.id = t.primary_artist
         WHERE pe.played_at >= ?1
           AND (pe.completed = 1 OR pe.listened_ms >= ?2)
         GROUP BY t.primary_artist
         ORDER BY COUNT(*) DESC, t.primary_artist
         LIMIT ?3
        "#,
    )
    .bind(since)
    .bind(QUALIFIED_PLAY_MS)
    .bind(VIBE_TOP_ARTISTS as i64)
    .fetch_all(pool)
    .await?;
    if top.is_empty() {
        return Ok(Vec::new());
    }

    let (energy, bpm): (Option<f64>, Option<f64>) = sqlx::query_as(
        r#"
        SELECT AVG(ta.energy), AVG(CASE WHEN ta.bpm > 0 THEN ta.bpm END)
          FROM play_event pe
          JOIN track_analysis ta ON ta.track_id = pe.track_id
         WHERE pe.played_at >= ?1
           AND (pe.completed = 1 OR pe.listened_ms >= ?2)
        "#,
    )
    .bind(since)
    .bind(QUALIFIED_PLAY_MS)
    .fetch_one(pool)
    .await?;
    let recent = RecentProfile { energy, bpm };

    // Each candidate artist, with the recent artist it comes from: an
    // artist played lately is its own anchor, a similar one takes the
    // first recent artist that lists it.
    let mut anchor_of: HashMap<i64, String> = HashMap::new();
    for (artist_id, name) in &top {
        anchor_of.insert(*artist_id, name.clone());
    }
    for (artist_id, name) in &top {
        for similar in cached_similar_library_ids(pool, *artist_id).await? {
            anchor_of.entry(similar).or_insert_with(|| name.clone());
        }
    }

    let artist_ids: Vec<i64> = anchor_of.keys().copied().collect();
    // Only `?` placeholders are interpolated; every value is bound.
    let placeholders = vec!["?"; artist_ids.len()].join(",");
    let sql = format!(
        r#"
        SELECT t.id, t.primary_artist, ta.energy, ta.bpm
          FROM track t
          LEFT JOIN track_analysis ta ON ta.track_id = t.id
         WHERE t.is_available = 1
           AND t.primary_artist IN ({placeholders})
           AND NOT EXISTS (
               SELECT 1 FROM play_event pe
                WHERE pe.track_id = t.id AND pe.played_at >= ?
           )
         LIMIT ?
        "#
    );
    let mut query =
        sqlx::query_as::<_, (i64, i64, Option<f64>, Option<f64>)>(sqlx::AssertSqlSafe(sql));
    for id in &artist_ids {
        query = query.bind(id);
    }
    let rows = query
        .bind(now_ms - VIBE_FRESH_DAYS * DAY_MS)
        .bind(VIBE_POOL_LIMIT)
        .fetch_all(pool)
        .await?;

    let mut scored: Vec<(f64, i64, i64)> = rows
        .into_iter()
        .map(|(track_id, artist_id, energy, bpm)| {
            (vibe_distance(recent, energy, bpm), track_id, artist_id)
        })
        .collect();
    scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let ranked = scored
        .into_iter()
        .map(|(_, track_id, artist_id)| Candidate {
            track_id,
            artist_id: Some(artist_id),
            anchor_artist: anchor_of.get(&artist_id).cloned(),
            ..Candidate::default()
        });
    Ok(shortlist(
        ranked,
        |c| c.artist_id,
        SHORTLIST_PER_ARTIST,
        SHORTLIST_LEN,
    ))
}

/// Never played, one track per artist (the latest added): by the
/// artists played most first, then the latest added, so a new library
/// still gets a suggestion. The one-per-artist cut is made in SQL, or the
/// most played artist's unplayed tracks would fill the whole pool.
async fn unplayed(pool: &SqlitePool) -> AppResult<Vec<Candidate>> {
    let rows: Vec<(i64, Option<i64>, i64, i64)> = sqlx::query_as(
        r#"
        WITH artist_plays AS (
            SELECT t.primary_artist AS artist_id, COUNT(*) AS plays
              FROM play_event pe
              JOIN track t ON t.id = pe.track_id
             WHERE t.primary_artist IS NOT NULL
               AND (pe.completed = 1 OR pe.listened_ms >= ?1)
             GROUP BY t.primary_artist
        )
        , unplayed AS (
            SELECT t.id, t.primary_artist, t.added_at,
                   ROW_NUMBER() OVER (
                       PARTITION BY COALESCE(t.primary_artist, -t.id)
                       ORDER BY t.added_at DESC, t.id
                   ) AS rank_in_artist
              FROM track t
             WHERE t.is_available = 1
               AND NOT EXISTS (SELECT 1 FROM play_event pe WHERE pe.track_id = t.id)
        )
        SELECT u.id, u.primary_artist, COALESCE(ap.plays, 0), u.added_at
          FROM unplayed u
          LEFT JOIN artist_plays ap ON ap.artist_id = u.primary_artist
         WHERE u.rank_in_artist = 1
         ORDER BY COALESCE(ap.plays, 0) DESC, u.added_at DESC, u.id
         LIMIT ?2
        "#,
    )
    .bind(QUALIFIED_PLAY_MS)
    .bind(POOL_LIMIT)
    .fetch_all(pool)
    .await?;
    let ranked = rows
        .into_iter()
        .map(|(track_id, artist_id, artist_plays, added_at)| Candidate {
            track_id,
            artist_id,
            artist_plays: Some(artist_plays),
            added_at: Some(added_at),
            ..Candidate::default()
        });
    Ok(shortlist(ranked, |c| c.artist_id, 1, SHORTLIST_LEN))
}
