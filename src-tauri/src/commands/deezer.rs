//! Tauri commands for Deezer metadata enrichment.
//!
//! Each command follows a **cache-first** strategy:
//! 1. Check if the local entity already has a `deezer_id`.
//! 2. If yes, check the `deezer_*` cache table for a non-expired entry.
//! 3. If the cache is valid, return it immediately (and resolve the local
//!    artwork path from the stored hash so the UI can render offline).
//! 4. Otherwise, search or fetch from the Deezer public API, download the
//!    artwork into the shared `metadata_artwork/` directory, upsert the cache
//!    row (with the hash) and link the `deezer_id` on the local entity.
//!
//! On any network error the command returns an **empty enrichment** (all
//! fields `None`) rather than propagating an error — the frontend can
//! display local data without interruption.

use chrono::Utc;
use serde::Serialize;

use crate::{
    commands::integration::read_lastfm_api_key,
    deezer::DeezerClient,
    error::AppResult,
    lastfm::LastfmClient,
    metadata_artwork,
    state::AppState,
};

/// TTL for cached Deezer entries: 30 days in milliseconds.
const CACHE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

// ── Album enrichment ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeezerAlbumEnrichment {
    pub deezer_id: Option<i64>,
    pub label: Option<String>,
    pub release_date: Option<String>,
    /// Remote Deezer CDN URL — kept as a fallback for the (rare) case where
    /// the local download failed. Frontend should prefer `cover_path`.
    pub cover_url: Option<String>,
    /// Absolute filesystem path to the locally-cached cover, or `None` if
    /// the download has not happened yet (or failed).
    pub cover_path: Option<String>,
}

impl DeezerAlbumEnrichment {
    fn empty() -> Self {
        Self {
            deezer_id: None,
            label: None,
            release_date: None,
            cover_url: None,
            cover_path: None,
        }
    }
}

#[tauri::command]
pub async fn enrich_album_deezer(
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<DeezerAlbumEnrichment> {
    let pool = state.require_profile_pool().await?;
    let artwork_dir = state.paths.metadata_artwork_dir.clone();
    let now = now_ms();

    // 1. Read the local album + its existing deezer_id.
    let local: Option<(String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT al.title, ar.name, al.deezer_id
           FROM album al LEFT JOIN artist ar ON ar.id = al.artist_id
          WHERE al.id = ?",
    )
    .bind(album_id)
    .fetch_optional(&pool)
    .await?;

    let Some((album_title, artist_name, existing_deezer_id)) = local else {
        return Ok(DeezerAlbumEnrichment::empty());
    };

    // 2. Cache hit?
    if let Some(did) = existing_deezer_id {
        let cached: Option<(
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT label, release_date, cover_url, cover_hash, expires_at
               FROM app.deezer_album WHERE deezer_id = ?",
        )
        .bind(did)
        .fetch_optional(&pool)
        .await?;

        if let Some((label, release_date, cover_url, cover_hash, expires_at)) = cached {
            if expires_at > now {
                let cover_path = cover_hash
                    .as_deref()
                    .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
                return Ok(DeezerAlbumEnrichment {
                    deezer_id: Some(did),
                    label,
                    release_date,
                    cover_url,
                    cover_path,
                });
            }
        }
    }

    // 3. Fetch from Deezer API.
    let client = DeezerClient::new();
    let hit = if let Some(did) = existing_deezer_id {
        match client.get_album(did).await {
            Ok(h) => Some(h),
            Err(err) => {
                tracing::warn!(?err, "Deezer get_album failed");
                return Ok(DeezerAlbumEnrichment {
                    deezer_id: Some(did),
                    ..DeezerAlbumEnrichment::empty()
                });
            }
        }
    } else {
        let query = match artist_name.as_deref() {
            Some(artist) => format!("{album_title} {artist}"),
            None => album_title.clone(),
        };
        match client.search_album(&query).await {
            Ok(hits) => hits.into_iter().next(),
            Err(err) => {
                tracing::warn!(?err, "Deezer search_album failed");
                return Ok(DeezerAlbumEnrichment::empty());
            }
        }
    };

    let Some(hit) = hit else {
        return Ok(DeezerAlbumEnrichment::empty());
    };

    let cover_url = hit.cover_xl.clone().or_else(|| hit.cover_big.clone());

    // 4. Download artwork into the shared cache (best-effort).
    let cover_hash = match cover_url.as_deref() {
        Some(url) => metadata_artwork::download_and_cache(url, &artwork_dir).await,
        None => None,
    };
    let cover_path = cover_hash
        .as_deref()
        .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));

    // 5. Upsert into cache (now stores the hash too).
    let expires = now + CACHE_TTL_MS;
    sqlx::query(
        "INSERT INTO app.deezer_album
            (deezer_id, title, release_date, cover_url, cover_hash, label, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deezer_id) DO UPDATE SET
           title = excluded.title,
           release_date = excluded.release_date,
           cover_url = excluded.cover_url,
           cover_hash = excluded.cover_hash,
           label = excluded.label,
           fetched_at = excluded.fetched_at,
           expires_at = excluded.expires_at",
    )
    .bind(hit.id)
    .bind(&hit.title)
    .bind(hit.release_date.as_deref())
    .bind(cover_url.as_deref())
    .bind(cover_hash.as_deref())
    .bind(hit.label.as_deref())
    .bind(now)
    .bind(expires)
    .execute(&pool)
    .await?;

    // 6. Link deezer_id on the local album.
    if existing_deezer_id.is_none() {
        sqlx::query("UPDATE album SET deezer_id = ? WHERE id = ?")
            .bind(hit.id)
            .bind(album_id)
            .execute(&pool)
            .await?;
    }

    Ok(DeezerAlbumEnrichment {
        deezer_id: Some(hit.id),
        label: hit.label,
        release_date: hit.release_date,
        cover_url,
        cover_path,
    })
}

// ── Artist enrichment ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeezerArtistEnrichment {
    pub deezer_id: Option<i64>,
    /// Remote Deezer CDN URL — fallback when the local download failed.
    pub picture_url: Option<String>,
    /// Absolute filesystem path to the locally-cached picture.
    pub picture_path: Option<String>,
    pub fans_count: Option<i64>,
    /// Short biography from Last.fm (if an API key is configured and
    /// the artist matches). HTML stripped.
    pub bio_short: Option<String>,
    /// Full biography from Last.fm. HTML stripped.
    pub bio_full: Option<String>,
}

impl DeezerArtistEnrichment {
    fn empty() -> Self {
        Self {
            deezer_id: None,
            picture_url: None,
            picture_path: None,
            fans_count: None,
            bio_short: None,
            bio_full: None,
        }
    }
}

#[tauri::command]
pub async fn enrich_artist_deezer(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<DeezerArtistEnrichment> {
    let pool = state.require_profile_pool().await?;
    let artwork_dir = state.paths.metadata_artwork_dir.clone();
    let now = now_ms();

    // 1. Read local artist.
    let local: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT name, deezer_id FROM artist WHERE id = ?")
            .bind(artist_id)
            .fetch_optional(&pool)
            .await?;

    let Some((artist_name, existing_deezer_id)) = local else {
        return Ok(DeezerArtistEnrichment::empty());
    };

    // 2. Cache hit? (includes bio fields populated by Last.fm in a
    //    previous enrichment pass)
    if let Some(did) = existing_deezer_id {
        let cached: Option<(
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT picture_url, picture_hash, fans_count, bio_short, bio_full, expires_at
               FROM app.deezer_artist WHERE deezer_id = ?",
        )
        .bind(did)
        .fetch_optional(&pool)
        .await?;

        if let Some((picture_url, picture_hash, fans_count, bio_short, bio_full, expires_at)) =
            cached
        {
            if expires_at > now {
                let picture_path = picture_hash
                    .as_deref()
                    .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
                return Ok(DeezerArtistEnrichment {
                    deezer_id: Some(did),
                    picture_url,
                    picture_path,
                    fans_count,
                    bio_short,
                    bio_full,
                });
            }
        }
    }

    // 3. Fetch from Deezer (picture + fans).
    let client = DeezerClient::new();
    let hit = if let Some(did) = existing_deezer_id {
        match client.get_artist(did).await {
            Ok(h) => Some(h),
            Err(err) => {
                tracing::warn!(?err, "Deezer get_artist failed");
                return Ok(DeezerArtistEnrichment {
                    deezer_id: Some(did),
                    ..DeezerArtistEnrichment::empty()
                });
            }
        }
    } else {
        match client.search_artist(&artist_name).await {
            Ok(hits) => {
                let canonical = artist_name.to_lowercase();
                hits.into_iter()
                    .find(|h| h.name.to_lowercase() == canonical)
            }
            Err(err) => {
                tracing::warn!(?err, "Deezer search_artist failed");
                return Ok(DeezerArtistEnrichment::empty());
            }
        }
    };

    let Some(hit) = hit else {
        return Ok(DeezerArtistEnrichment::empty());
    };

    // 4. Fetch bio from Last.fm if an API key is configured. Network
    //    failures and missing matches are non-fatal — we still persist
    //    the Deezer portion so the next refresh doesn't spam the
    //    network.
    let (bio_short, bio_full) = match read_lastfm_api_key(&state).await? {
        Some(api_key) => {
            let lastfm = LastfmClient::new();
            match lastfm.artist_get_info(&artist_name, &api_key).await {
                Ok(Some(info)) => (info.bio_summary, info.bio_full),
                Ok(None) => (None, None),
                Err(err) => {
                    tracing::warn!(?err, "Last.fm artist_get_info failed");
                    (None, None)
                }
            }
        }
        None => (None, None),
    };

    let picture_url = hit.picture_xl.clone().or_else(|| hit.picture_big.clone());

    // 5. Download artwork into the shared cache (best-effort).
    let picture_hash = match picture_url.as_deref() {
        Some(url) => metadata_artwork::download_and_cache(url, &artwork_dir).await,
        None => None,
    };
    let picture_path = picture_hash
        .as_deref()
        .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));

    // 6. Upsert into the metadata cache (both Deezer and Last.fm
    //    fields live in the historically-named `deezer_artist` table,
    //    now stored in app.db so all profiles share the same cache).
    let expires = now + CACHE_TTL_MS;
    sqlx::query(
        "INSERT INTO app.deezer_artist
            (deezer_id, name, picture_url, picture_hash, fans_count, bio_short, bio_full, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deezer_id) DO UPDATE SET
           name = excluded.name,
           picture_url = excluded.picture_url,
           picture_hash = excluded.picture_hash,
           fans_count = excluded.fans_count,
           bio_short = excluded.bio_short,
           bio_full = excluded.bio_full,
           fetched_at = excluded.fetched_at,
           expires_at = excluded.expires_at",
    )
    .bind(hit.id)
    .bind(&hit.name)
    .bind(picture_url.as_deref())
    .bind(picture_hash.as_deref())
    .bind(hit.nb_fan)
    .bind(bio_short.as_deref())
    .bind(bio_full.as_deref())
    .bind(now)
    .bind(expires)
    .execute(&pool)
    .await?;

    // 7. Link deezer_id on the local artist.
    if existing_deezer_id.is_none() {
        sqlx::query("UPDATE artist SET deezer_id = ? WHERE id = ?")
            .bind(hit.id)
            .bind(artist_id)
            .execute(&pool)
            .await?;
    }

    Ok(DeezerArtistEnrichment {
        deezer_id: Some(hit.id),
        picture_url,
        picture_path,
        fans_count: hit.nb_fan,
        bio_short,
        bio_full,
    })
}
