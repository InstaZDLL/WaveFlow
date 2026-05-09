//! Similar-artists discovery.
//!
//! Two upstream sources, queried in cascade:
//!   1. **Last.fm `artist.getSimilar`** — preferred. Returns affinity
//!      scores, but requires the user to have configured an API key.
//!   2. **Deezer `/artist/{id}/related`** — fallback when Last.fm has
//!      no key or returned an empty list. No score (results are
//!      ordered by Deezer's own ranking) so we synthesize a decreasing
//!      `match_score` from the position.
//!
//! Both sources are cached in `app.lastfm_similar` (despite the name)
//! for 30 days, keyed by the source artist's canonical name. Each
//! entry returned to the UI is augmented with a `library_artist_id`
//! when a name match exists in the active profile — enables a "in your
//! library" badge and a click-to-navigate behaviour.

use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    commands::{integration::read_lastfm_api_key, scan::canonical_name},
    deezer::DeezerClient,
    error::AppResult,
    lastfm::LastfmClient,
    metadata_artwork,
    state::AppState,
};

/// 30-day TTL for similar-artists cache, in milliseconds.
const CACHE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Maximum number of suggestions surfaced to the UI.
const RESULT_LIMIT: usize = 12;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarArtistDto {
    pub name: String,
    /// 0-1 affinity. From Last.fm directly; synthesised from rank for
    /// Deezer (`1.0 - i / N`) so the UI can sort uniformly.
    pub match_score: f32,
    /// Remote URL. Last.fm images are usually generic placeholders so
    /// the UI should prefer `picture_path` whenever available.
    pub picture_url: Option<String>,
    /// Local cached picture (Deezer-sourced). `None` when the artist
    /// isn't in the user's library and we haven't pre-fetched their
    /// metadata yet.
    pub picture_path: Option<String>,
    /// Resolved local artist row. `None` when the suggested artist
    /// isn't in the user's library — the UI can either grey out the
    /// card or open an external Deezer link.
    pub library_artist_id: Option<i64>,
    /// `lastfm` or `deezer` — surfaced for transparency / debugging,
    /// not used by the default UI.
    pub source: String,
}

#[tauri::command]
pub async fn get_similar_artists(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<Vec<SimilarArtistDto>> {
    let pool = state.require_profile_pool().await?;
    let artwork_dir = state.paths.metadata_artwork_dir.clone();
    let api_key = read_lastfm_api_key(&state).await?;

    // 1. Resolve the source artist's name + Deezer ID (used by the
    //    Deezer fallback path).
    let local: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT name, deezer_id FROM artist WHERE id = ?")
            .bind(artist_id)
            .fetch_optional(&pool)
            .await?;
    let Some((source_name, source_deezer_id)) = local else {
        return Ok(Vec::new());
    };
    let source_canonical = canonical_name(&source_name);
    if source_canonical.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Cache check.
    let now = now_ms();
    let cached: Option<(String, i64)> = sqlx::query_as(
        "SELECT payload, expires_at FROM app.lastfm_similar
          WHERE name_canonical = ?",
    )
    .bind(&source_canonical)
    .fetch_optional(&pool)
    .await?;

    let raw: Vec<RawSimilar> = if let Some((payload, expires_at)) = cached {
        if expires_at > now {
            serde_json::from_str(&payload).unwrap_or_default()
        } else {
            fetch_and_cache(
                &pool,
                api_key.as_deref(),
                &source_name,
                &source_canonical,
                source_deezer_id,
                now,
            )
            .await?
        }
    } else {
        fetch_and_cache(
            &pool,
            api_key.as_deref(),
            &source_name,
            &source_canonical,
            source_deezer_id,
            now,
        )
        .await?
    };

    if raw.is_empty() {
        return Ok(Vec::new());
    }

    // 3. Resolve each suggestion against the active profile so the UI
    //    can badge entries the user already owns. Single batched query
    //    instead of N+1 round-trips.
    let canonicals: Vec<String> = raw.iter().map(|r| canonical_name(&r.name)).collect();
    let placeholders = canonicals
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let mut local_map: HashMap<String, (i64, Option<String>)> = HashMap::new();
    if !canonicals.is_empty() {
        let sql = format!(
            "SELECT a.id, a.canonical_name, ma.picture_hash
               FROM artist a
               LEFT JOIN app.metadata_artist ma ON ma.deezer_id = a.deezer_id
              WHERE a.canonical_name IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, (i64, String, Option<String>)>(&sql);
        for c in &canonicals {
            q = q.bind(c);
        }
        for (id, canon, hash) in q.fetch_all(&pool).await? {
            local_map.insert(canon, (id, hash));
        }
    }

    let out = raw
        .into_iter()
        .take(RESULT_LIMIT)
        .map(|r| {
            let canon = canonical_name(&r.name);
            let local = local_map.get(&canon);
            let picture_path = local
                .and_then(|(_, hash)| hash.as_deref())
                .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
            SimilarArtistDto {
                name: r.name,
                match_score: r.match_score,
                picture_url: r.picture_url,
                picture_path,
                library_artist_id: local.map(|(id, _)| *id),
                source: r.source,
            }
        })
        .collect();
    Ok(out)
}

/// Populate `app.lastfm_similar` for `artist_id` when the cache row is
/// missing or stale. Used by `start_radio` so that the very first
/// "Démarrer la radio" click on a new artist still pulls similar
/// artists, instead of degrading to a seed-artist-only queue (which
/// looks like the radio "did nothing" when the seed has few siblings).
///
/// No-op on success: the cache row is the side-effect. Returns `Ok(())`
/// even when the upstream lookup fails — radio is allowed to degrade
/// gracefully, it should never block the user click on a network
/// hiccup.
pub async fn ensure_similar_cached(
    state: &AppState,
    artist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let local: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT name, deezer_id FROM artist WHERE id = ?")
            .bind(artist_id)
            .fetch_optional(&pool)
            .await?;
    let Some((name, deezer_id)) = local else {
        return Ok(());
    };
    let canonical = canonical_name(&name);
    if canonical.is_empty() {
        return Ok(());
    }

    let now = now_ms();
    let cached: Option<(i64,)> = sqlx::query_as(
        "SELECT expires_at FROM app.lastfm_similar WHERE name_canonical = ?",
    )
    .bind(&canonical)
    .fetch_optional(&pool)
    .await?;
    if cached.map(|(exp,)| exp > now).unwrap_or(false) {
        return Ok(());
    }

    let api_key = read_lastfm_api_key(state).await?;
    let _ = fetch_and_cache(&pool, api_key.as_deref(), &name, &canonical, deezer_id, now)
        .await;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RawSimilar {
    name: String,
    match_score: f32,
    picture_url: Option<String>,
    source: String,
}

/// Query the upstream provider(s) and persist the result. Last.fm wins
/// when an API key is configured AND it returns at least one entry —
/// otherwise we fall through to Deezer's `/artist/{id}/related`.
async fn fetch_and_cache(
    pool: &SqlitePool,
    api_key: Option<&str>,
    source_name: &str,
    source_canonical: &str,
    source_deezer_id: Option<i64>,
    now: i64,
) -> AppResult<Vec<RawSimilar>> {
    let (raw, source_label) = match try_lastfm(api_key, source_name).await {
        Some(list) if !list.is_empty() => (list, "lastfm"),
        _ => match try_deezer(source_deezer_id, source_name).await {
            Some(list) => (list, "deezer"),
            None => (Vec::new(), "deezer"),
        },
    };

    let payload = serde_json::to_string(&raw).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO app.lastfm_similar
            (name_canonical, payload, source, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(name_canonical) DO UPDATE SET
            payload    = excluded.payload,
            source     = excluded.source,
            fetched_at = excluded.fetched_at,
            expires_at = excluded.expires_at",
    )
    .bind(source_canonical)
    .bind(&payload)
    .bind(source_label)
    .bind(now)
    .bind(now + CACHE_TTL_MS)
    .execute(pool)
    .await?;

    Ok(raw)
}

async fn try_lastfm(api_key: Option<&str>, source_name: &str) -> Option<Vec<RawSimilar>> {
    let key = api_key?;
    let client = LastfmClient::new();
    match client
        .artist_get_similar(source_name, key, RESULT_LIMIT as u32)
        .await
    {
        Ok(list) => Some(
            list.into_iter()
                .map(|s| RawSimilar {
                    name: s.name,
                    match_score: s.match_score,
                    picture_url: s.picture_url,
                    source: "lastfm".into(),
                })
                .collect(),
        ),
        Err(err) => {
            tracing::warn!(?err, "Last.fm artist.getSimilar failed");
            None
        }
    }
}

async fn try_deezer(deezer_id: Option<i64>, source_name: &str) -> Option<Vec<RawSimilar>> {
    let client = DeezerClient::new();
    let target_id = match deezer_id {
        Some(id) => id,
        None => match client.search_artist(source_name).await {
            Ok(hits) => {
                let canon = source_name.to_lowercase();
                hits.into_iter().find(|h| h.name.to_lowercase() == canon)?.id
            }
            Err(err) => {
                tracing::warn!(?err, "Deezer search_artist failed");
                return None;
            }
        },
    };
    match client.get_related_artists(target_id).await {
        Ok(list) => {
            let n = list.len().max(1) as f32;
            Some(
                list.into_iter()
                    .enumerate()
                    .map(|(i, h)| RawSimilar {
                        name: h.name,
                        // Synthesize a decreasing 1.0 → ~0 score from
                        // the Deezer ranking so the UI can sort
                        // uniformly with Last.fm output.
                        match_score: 1.0 - (i as f32) / n,
                        picture_url: h.picture_big.or(h.picture_medium),
                        source: "deezer".into(),
                    })
                    .collect(),
            )
        }
        Err(err) => {
            tracing::warn!(?err, "Deezer get_related_artists failed");
            None
        }
    }
}
