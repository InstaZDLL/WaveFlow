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

use waveflow_core::metadata::{deezer::DeezerClient, lastfm::LastfmClient};

use crate::{
    commands::{integration::read_lastfm_api_key, scan::canonical_name},
    error::AppResult,
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

    // Offline mode: hand back whatever the cache holds (even if
    // stale) and never trigger a network refresh. Empty when no
    // cache row exists yet.
    let raw: Vec<RawSimilar> = if crate::offline::is_offline() {
        match cached {
            Some((payload, _)) => serde_json::from_str(&payload).unwrap_or_default(),
            None => Vec::new(),
        }
    } else if let Some((payload, expires_at)) = cached {
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
    let placeholders = canonicals.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let mut local_map: HashMap<String, (i64, Option<String>)> = HashMap::new();
    if !canonicals.is_empty() {
        let sql = format!(
            "SELECT a.id, a.canonical_name, ma.picture_hash
               FROM artist a
               LEFT JOIN app.metadata_artist ma ON ma.deezer_id = a.deezer_id
              WHERE a.canonical_name IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, (i64, String, Option<String>)>(sqlx::AssertSqlSafe(sql));
        for c in &canonicals {
            q = q.bind(c);
        }
        for (id, canon, hash) in q.fetch_all(&pool).await? {
            local_map.insert(canon, (id, hash));
        }
    }

    // 4. Picture enrichment via Deezer. Last.fm's `artist.getSimilar`
    //    returns the same generic star placeholder URL for every entry
    //    (their artist-image API was retired in 2019), so without this
    //    step Last.fm-configured users get a sea of grey stars for
    //    every similar artist that isn't already in their library.
    //    This step is a no-op when offline mode is on.
    let metadata_map =
        enrich_with_deezer_pictures(&pool, &artwork_dir, &raw, now).await;

    let out = raw
        .into_iter()
        .take(RESULT_LIMIT)
        .map(|r| {
            let canon = canonical_name(&r.name);
            let local = local_map.get(&canon);
            let meta = metadata_map.get(&canon);
            // Picture-path priority: profile-DB Deezer hash (works
            // offline) → cross-profile `metadata_artist` hash (filled
            // by the enrichment step above for entries not in the
            // library) → no local fallback, UI uses `picture_url`.
            let picture_path = local
                .and_then(|(_, hash)| hash.as_deref())
                .or_else(|| meta.and_then(|(_, hash)| hash.as_deref()))
                .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
            // Prefer the Deezer URL over Last.fm's placeholder when
            // both are present. Falls back to whatever the upstream
            // gave us so a Deezer-fetch failure still surfaces *some*
            // remote URL (good enough for the in-library badge case).
            let picture_url = meta
                .and_then(|(url, _)| url.clone())
                .or(r.picture_url);
            SimilarArtistDto {
                name: r.name,
                match_score: r.match_score,
                picture_url,
                picture_path,
                library_artist_id: local.map(|(id, _)| *id),
                source: r.source,
            }
        })
        .collect();
    Ok(out)
}

/// Hard cap on concurrent Deezer `search_artist` round-trips kicked off
/// by the enrichment fan-out. Matches `RESULT_LIMIT` — the displayed
/// list is capped at 12, so there's never a reason to overlap more than
/// 12 outbound requests for a single artist-page click.
const CONCURRENCY_LIMIT: usize = RESULT_LIMIT;

/// Backfill `app.metadata_artist` for every name in `raw` that we don't
/// already have a non-expired cache row for, then return a
/// `canonical_name → (picture_url, picture_hash)` lookup map ready to
/// be merged into the outgoing DTOs.
///
/// The cache is the cross-profile `app.metadata_artist` so the work is
/// shared with other features (the `ArtistDetailView` Deezer enrichment,
/// the Wrapped year-in-review, etc.). Cache misses fan out to Deezer
/// `search_artist` through a buffered stream bounded by
/// [`CONCURRENCY_LIMIT`], and the miss set is itself trimmed to
/// [`RESULT_LIMIT`] so we never spend network on entries that the
/// final `.take(RESULT_LIMIT)` will drop anyway.
async fn enrich_with_deezer_pictures(
    pool: &SqlitePool,
    artwork_dir: &std::path::Path,
    raw: &[RawSimilar],
    now: i64,
) -> HashMap<String, (Option<String>, Option<String>)> {
    use futures::stream::StreamExt;

    let mut map: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    if raw.is_empty() {
        return map;
    }

    // Pull every non-expired metadata row in one round-trip then filter
    // in Rust against `canonical_name()`. We deliberately don't push the
    // canonicalisation into SQL (`LOWER(TRIM(name))` would mismatch
    // names like "AC/DC" → "acdc" or "P!nk" → "pnk" because SQLite's
    // standard build has no REGEXP function), and SQLite has no
    // user-defined alphanumeric filter. The cache is bounded by the
    // user's library size + enrichment activity (~hundreds to a few
    // thousand rows in steady state), so a full table scan + Rust-side
    // filter is sub-millisecond and removes the canonicalisation
    // mismatch bug. Promote to a stored `canonical_name` column with an
    // index if profiling ever flags this query.
    let canon_targets: std::collections::HashSet<String> = raw
        .iter()
        .map(|r| canonical_name(&r.name))
        .filter(|c| !c.is_empty())
        .collect();

    // Offline mode reads the cache *without* the TTL filter — we have
    // no way to refresh anyway, and any locally cached picture (the
    // shared `metadata_artwork/<hash>.jpg` blob is keyed by blake3 and
    // never expires) is strictly better than a grey star. Online mode
    // applies `expires_at > now` so an expired row falls through to a
    // Deezer refresh.
    let offline = crate::offline::is_offline();
    let cache_result = if offline {
        sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT name, picture_url, picture_hash FROM app.metadata_artist",
        )
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT name, picture_url, picture_hash
               FROM app.metadata_artist
              WHERE expires_at > ?",
        )
        .bind(now)
        .fetch_all(pool)
        .await
    };
    match cache_result {
        Ok(rows) => {
            for (name, picture_url, picture_hash) in rows {
                let canon = canonical_name(&name);
                if canon_targets.contains(&canon) {
                    map.insert(canon, (picture_url, picture_hash));
                }
            }
        }
        Err(err) => {
            tracing::warn!(
                ?err,
                "similar-artist picture cache lookup failed — falling through to Deezer"
            );
        }
    }

    // Network refresh is out of the question when offline — short-circuit
    // here so neither the miss computation nor the Deezer fan-out runs.
    if offline {
        return map;
    }

    // Trim misses to the same RESULT_LIMIT window the caller's `.take`
    // applies. `raw` arrives ordered by upstream affinity (Last.fm
    // match score or Deezer ranking) so the first slice is exactly
    // the entries that'll end up on screen. Collect owned names so the
    // downstream stream owns its inputs — avoids HRTB lifetime grief
    // with `buffer_unordered` borrowing back into `raw`.
    let miss_names: Vec<String> = raw[..raw.len().min(RESULT_LIMIT)]
        .iter()
        .filter_map(|r| {
            let canon = canonical_name(&r.name);
            if !canon.is_empty() && !map.contains_key(&canon) {
                Some(r.name.clone())
            } else {
                None
            }
        })
        .collect();
    if miss_names.is_empty() {
        return map;
    }

    let client = DeezerClient::new();
    let expires = now + CACHE_TTL_MS;
    let fetched: Vec<(String, Option<waveflow_core::metadata::deezer::DeezerArtistHit>)> =
        futures::stream::iter(miss_names.into_iter().map(|name| {
            let client = client.clone();
            async move {
                let canon = canonical_name(&name);
                let hit = match client.search_artist(&name).await {
                    Ok(hits) => hits
                        .into_iter()
                        .find(|h| canonical_name(&h.name) == canon),
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            artist = %name,
                            "Deezer search for similar-artist enrichment failed"
                        );
                        None
                    }
                };
                (canon, hit)
            }
        }))
        .buffer_unordered(CONCURRENCY_LIMIT)
        .collect()
        .await;

    for (canon, hit) in fetched {
        let Some(hit) = hit else { continue };
        let picture_url = hit.picture_xl.clone().or_else(|| hit.picture_big.clone());
        let picture_hash = match picture_url.as_deref() {
            Some(url) => metadata_artwork::download_and_cache(url, artwork_dir).await,
            None => None,
        };
        // Persist the lookup so the next request for the SAME similar
        // artist (or a different page asking about them) reuses this
        // result instead of poking Deezer again. ON CONFLICT also
        // refreshes the expiry for entries that already existed but
        // were expired — same shape as `enrich_artist_deezer`.
        if let Err(err) = sqlx::query(
            "INSERT INTO app.metadata_artist
                (deezer_id, name, picture_url, picture_hash, fetched_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(deezer_id) DO UPDATE SET
               name = excluded.name,
               picture_url = excluded.picture_url,
               picture_hash = excluded.picture_hash,
               fetched_at = excluded.fetched_at,
               expires_at = excluded.expires_at",
        )
        .bind(hit.id)
        .bind(&hit.name)
        .bind(picture_url.as_deref())
        .bind(picture_hash.as_deref())
        .bind(now)
        .bind(expires)
        .execute(pool)
        .await
        {
            tracing::warn!(
                ?err,
                artist = %hit.name,
                "metadata_artist upsert failed during similar-artist enrichment"
            );
        }

        map.insert(canon, (picture_url, picture_hash));
    }

    map
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
pub async fn ensure_similar_cached(state: &AppState, artist_id: i64) -> AppResult<()> {
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
    let cached: Option<(i64,)> =
        sqlx::query_as("SELECT expires_at FROM app.lastfm_similar WHERE name_canonical = ?")
            .bind(&canonical)
            .fetch_optional(&pool)
            .await?;
    if cached.map(|(exp,)| exp > now).unwrap_or(false) {
        return Ok(());
    }

    let api_key = read_lastfm_api_key(state).await?;
    let _ = fetch_and_cache(&pool, api_key.as_deref(), &name, &canonical, deezer_id, now).await;
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
                hits.into_iter()
                    .find(|h| h.name.to_lowercase() == canon)?
                    .id
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
