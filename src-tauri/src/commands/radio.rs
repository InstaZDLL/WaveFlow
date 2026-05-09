//! "Start radio from this track" — Spotify-style auto-generated queue.
//!
//! Builds a track list of ~40 entries by mixing:
//!   - the seed track itself at position 0,
//!   - other tracks by the seed's primary artist (capped at 6),
//!   - tracks by the artist's similar artists (resolved through the
//!     `app.lastfm_similar` cache, falling back to seed-artist-only
//!     when the cache is empty).
//!
//! When the seed track has a stored BPM in `track_analysis`, candidates
//! are soft-filtered to a ±18 BPM window before fallback — keeps a
//! chillout seed from suddenly switching to drum-and-bass. The filter
//! only applies when at least 12 candidates survive it; otherwise we
//! disable the constraint to avoid yielding a 5-track radio.
//!
//! Returns the ordered `Vec<i64>` of track IDs. The frontend hands this
//! to `player_play_tracks` with `source_type = "radio"` so play_event
//! rows get tagged correctly for stats.

use std::collections::HashSet;

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    commands::scan::canonical_name,
    error::{AppError, AppResult},
    state::AppState,
};

/// Target size of the generated radio. Spotify ships ~50, Apple Music
/// ~25 — 40 hits the sweet spot of "long enough to forget about it"
/// without being so long that the tail drifts off-style.
const TARGET_LEN: usize = 40;

/// Hard cap on tracks pulled from the seed artist (primary artist of
/// the seed track). Without this cap, an obscure seed could yield a
/// queue made of 95 % the same artist — defeats the discovery angle.
const SEED_ARTIST_CAP: usize = 6;

/// Number of similar artists pulled from the cache. Higher = more
/// variety but also more risk of irrelevant suggestions deep in the
/// list (Last.fm's tail gets noisy fast).
const SIMILAR_ARTIST_CAP: usize = 8;

/// Half-window (BPM) around the seed track's tempo for soft filtering.
/// ±18 keeps within the same "groove family" most of the time; small
/// enough to feel intentional, wide enough not to starve the pool.
const BPM_WINDOW: f64 = 18.0;

/// Minimum number of survivors required before BPM filtering is
/// allowed to take effect — below this we drop the constraint.
const BPM_MIN_SURVIVORS: usize = 12;

#[derive(Debug, Deserialize, Default)]
struct RawSimilar {
    name: String,
    #[allow(dead_code)]
    match_score: f32,
}

#[tauri::command]
pub async fn start_radio(
    state: tauri::State<'_, AppState>,
    seed_track_id: i64,
) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;

    // 1. Resolve the seed.
    let row: Option<(i64, Option<f64>)> = sqlx::query_as(
        r#"
        SELECT t.primary_artist, ta.bpm
          FROM track t
          LEFT JOIN track_analysis ta ON ta.track_id = t.id
         WHERE t.id = ? AND t.is_available = 1
        "#,
    )
    .bind(seed_track_id)
    .fetch_optional(&pool)
    .await?;
    let (seed_artist_id, seed_bpm) = row.ok_or_else(|| {
        AppError::Other(format!("seed track {seed_track_id} not found"))
    })?;

    // 2. Look up the seed artist's similar artists from cache. Anyone
    //    who's visited the artist's detail page once will have this
    //    populated; otherwise we fall back to seed-artist-only.
    let similar_artist_ids =
        cached_similar_library_ids(&pool, seed_artist_id).await?;

    // 3. Build the candidate pool. Seed artist always included so the
    //    radio doesn't drift off-vibe even with zero similar matches.
    let mut artist_ids: Vec<i64> = vec![seed_artist_id];
    artist_ids.extend(similar_artist_ids.iter().copied().take(SIMILAR_ARTIST_CAP));

    // 4. Pull every available track by these artists, keeping the
    //    play-count signal for ordering. Capped at 200 to keep the
    //    in-memory shuffle bounded.
    let candidates = pick_candidate_tracks(&pool, &artist_ids).await?;

    // 5. Apply the BPM soft filter when meaningful.
    let filtered: Vec<TrackCandidate> = if let Some(bpm) = seed_bpm {
        let lo = bpm - BPM_WINDOW;
        let hi = bpm + BPM_WINDOW;
        let in_window: Vec<TrackCandidate> = candidates
            .iter()
            .filter(|c| {
                c.bpm.map(|b| b >= lo && b <= hi).unwrap_or(false)
            })
            .cloned()
            .collect();
        if in_window.len() >= BPM_MIN_SURVIVORS {
            in_window
        } else {
            candidates
        }
    } else {
        candidates
    };

    // 6. Partition into seed-artist vs others, then assemble.
    let (seed_artist_tracks, other_tracks): (Vec<_>, Vec<_>) =
        filtered.into_iter().partition(|c| c.primary_artist == seed_artist_id);

    let mut rng_state = seed_track_id as u64 ^ 0x9E37_79B9_7F4A_7C15;
    let mut seed_artist_shuffled = seed_artist_tracks;
    fisher_yates(&mut seed_artist_shuffled, &mut rng_state);
    let mut other_shuffled = other_tracks;
    fisher_yates(&mut other_shuffled, &mut rng_state);

    // 7. Compose: seed first, then a small slice of seed-artist
    //    tracks (skipping the seed itself), then everything else.
    let mut out: Vec<i64> = Vec::with_capacity(TARGET_LEN);
    let mut seen: HashSet<i64> = HashSet::new();
    out.push(seed_track_id);
    seen.insert(seed_track_id);

    for c in seed_artist_shuffled
        .into_iter()
        .filter(|c| c.track_id != seed_track_id)
        .take(SEED_ARTIST_CAP)
    {
        if seen.insert(c.track_id) {
            out.push(c.track_id);
        }
    }

    for c in other_shuffled {
        if out.len() >= TARGET_LEN {
            break;
        }
        if seen.insert(c.track_id) {
            out.push(c.track_id);
        }
    }

    Ok(out)
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct TrackCandidate {
    track_id: i64,
    primary_artist: i64,
    bpm: Option<f64>,
}

async fn pick_candidate_tracks(
    pool: &SqlitePool,
    artist_ids: &[i64],
) -> AppResult<Vec<TrackCandidate>> {
    if artist_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat("?")
        .take(artist_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    // ORDER BY play_count DESC favours tracks the user actually
    // listens to — feels more like "your" radio than a cold-start
    // recommender.
    let sql = format!(
        r#"
        SELECT t.id            AS track_id,
               t.primary_artist AS primary_artist,
               ta.bpm           AS bpm
          FROM track t
          LEFT JOIN track_analysis ta ON ta.track_id = t.id
          LEFT JOIN play_event pe     ON pe.track_id = t.id
         WHERE t.primary_artist IN ({placeholders})
           AND t.is_available = 1
         GROUP BY t.id
         ORDER BY COUNT(pe.id) DESC, t.id ASC
         LIMIT 200
        "#
    );
    let mut q = sqlx::query_as::<_, TrackCandidate>(&sql);
    for id in artist_ids {
        q = q.bind(*id);
    }
    Ok(q.fetch_all(pool).await?)
}

/// Read the `app.lastfm_similar` cache for the seed artist and resolve
/// each suggestion to a library artist ID. Stale entries (past
/// `expires_at`) are still used — a slightly outdated similarity list
/// beats no radio at all. The cache is populated on demand by the
/// `get_similar_artists` command (artist detail page).
async fn cached_similar_library_ids(
    pool: &SqlitePool,
    seed_artist_id: i64,
) -> AppResult<Vec<i64>> {
    let seed_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM artist WHERE id = ?")
            .bind(seed_artist_id)
            .fetch_optional(pool)
            .await?;
    let Some(name) = seed_name else {
        return Ok(Vec::new());
    };
    let canonical = canonical_name(&name);
    if canonical.is_empty() {
        return Ok(Vec::new());
    }

    let payload: Option<String> = sqlx::query_scalar(
        "SELECT payload FROM app.lastfm_similar WHERE name_canonical = ?",
    )
    .bind(&canonical)
    .fetch_optional(pool)
    .await?;
    let Some(payload) = payload else {
        return Ok(Vec::new());
    };
    let raw: Vec<RawSimilar> = serde_json::from_str(&payload).unwrap_or_default();
    if raw.is_empty() {
        return Ok(Vec::new());
    }

    // Batch-resolve names to artist IDs. Single query, preserves
    // input order so high-affinity matches stay first.
    let canonicals: Vec<String> = raw.iter().map(|r| canonical_name(&r.name)).collect();
    let placeholders = canonicals
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, canonical_name FROM artist WHERE canonical_name IN ({placeholders})"
    );
    let mut q = sqlx::query_as::<_, (i64, String)>(&sql);
    for c in &canonicals {
        q = q.bind(c);
    }
    let rows = q.fetch_all(pool).await?;
    let lookup: std::collections::HashMap<String, i64> = rows.into_iter()
        .map(|(id, c)| (c, id))
        .collect();

    let mut out = Vec::with_capacity(canonicals.len());
    for c in canonicals {
        if let Some(&id) = lookup.get(&c) {
            if id != seed_artist_id && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// In-place Fisher-Yates shuffle backed by a tiny xorshift64 PRNG.
/// Deterministic per seed_track_id so re-launching the same radio
/// twice in a row gives a stable order — feels less random-jumpy than
/// a fresh entropy each call.
fn fisher_yates<T>(slice: &mut [T], state: &mut u64) {
    for i in (1..slice.len()).rev() {
        let j = (xorshift64(state) as usize) % (i + 1);
        slice.swap(i, j);
    }
}

fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    if x == 0 {
        x = 0xDEAD_BEEF_CAFE_BABE;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}
