//! Tauri commands for Deezer metadata enrichment.
//!
//! Each command follows a **cache-first** strategy:
//! 1. Check if the local entity already has a `deezer_id`.
//! 2. If yes, check the `deezer_*` cache table for a non-expired entry.
//! 3. If the cache is valid, return it immediately.
//! 4. Otherwise, search or fetch from the Deezer public API, upsert into
//!    the cache table, and link the `deezer_id` on the local entity.
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
    pub cover_url: Option<String>,
}

impl DeezerAlbumEnrichment {
    fn empty() -> Self {
        Self {
            deezer_id: None,
            label: None,
            release_date: None,
            cover_url: None,
        }
    }
}

#[tauri::command]
pub async fn enrich_album_deezer(
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<DeezerAlbumEnrichment> {
    let pool = state.require_profile_pool().await?;
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
        let cached: Option<(Option<String>, Option<String>, Option<String>, i64)> = sqlx::query_as(
            "SELECT label, release_date, cover_url, expires_at FROM deezer_album WHERE deezer_id = ?",
        )
        .bind(did)
        .fetch_optional(&pool)
        .await?;

        if let Some((label, release_date, cover_url, expires_at)) = cached {
            if expires_at > now {
                return Ok(DeezerAlbumEnrichment {
                    deezer_id: Some(did),
                    label,
                    release_date,
                    cover_url,
                });
            }
        }
    }

    // 3. Fetch from Deezer API.
    let client = DeezerClient::new();
    let hit = if let Some(did) = existing_deezer_id {
        // We already know the Deezer ID — direct fetch.
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
        // Search by "album title artist name".
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

    // 4. Upsert into cache.
    let expires = now + CACHE_TTL_MS;
    sqlx::query(
        "INSERT INTO deezer_album (deezer_id, title, release_date, cover_url, label, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deezer_id) DO UPDATE SET
           title = excluded.title,
           release_date = excluded.release_date,
           cover_url = excluded.cover_url,
           label = excluded.label,
           fetched_at = excluded.fetched_at,
           expires_at = excluded.expires_at",
    )
    .bind(hit.id)
    .bind(&hit.title)
    .bind(hit.release_date.as_deref())
    .bind(hit.cover_xl.as_deref().or(hit.cover_big.as_deref()))
    .bind(hit.label.as_deref())
    .bind(now)
    .bind(expires)
    .execute(&pool)
    .await?;

    // 5. Link deezer_id on the local album.
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
        cover_url: hit.cover_xl.or(hit.cover_big),
    })
}

// ── Artist enrichment ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeezerArtistEnrichment {
    pub deezer_id: Option<i64>,
    pub picture_url: Option<String>,
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
            Option<i64>,
            Option<String>,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT picture_url, fans_count, bio_short, bio_full, expires_at
               FROM deezer_artist WHERE deezer_id = ?",
        )
        .bind(did)
        .fetch_optional(&pool)
        .await?;

        if let Some((picture_url, fans_count, bio_short, bio_full, expires_at)) = cached {
            if expires_at > now {
                return Ok(DeezerArtistEnrichment {
                    deezer_id: Some(did),
                    picture_url,
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

    // 5. Upsert into the metadata cache (both Deezer and Last.fm
    //    fields live in the historically-named `deezer_artist` table).
    let expires = now + CACHE_TTL_MS;
    sqlx::query(
        "INSERT INTO deezer_artist
            (deezer_id, name, picture_url, fans_count, bio_short, bio_full, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deezer_id) DO UPDATE SET
           name = excluded.name,
           picture_url = excluded.picture_url,
           fans_count = excluded.fans_count,
           bio_short = excluded.bio_short,
           bio_full = excluded.bio_full,
           fetched_at = excluded.fetched_at,
           expires_at = excluded.expires_at",
    )
    .bind(hit.id)
    .bind(&hit.name)
    .bind(hit.picture_xl.as_deref().or(hit.picture_big.as_deref()))
    .bind(hit.nb_fan)
    .bind(bio_short.as_deref())
    .bind(bio_full.as_deref())
    .bind(now)
    .bind(expires)
    .execute(&pool)
    .await?;

    // 6. Link deezer_id on the local artist.
    if existing_deezer_id.is_none() {
        sqlx::query("UPDATE artist SET deezer_id = ? WHERE id = ?")
            .bind(hit.id)
            .bind(artist_id)
            .execute(&pool)
            .await?;
    }

    Ok(DeezerArtistEnrichment {
        deezer_id: Some(hit.id),
        picture_url: hit.picture_xl.or(hit.picture_big),
        fans_count: hit.nb_fan,
        bio_short,
        bio_full,
    })
}
