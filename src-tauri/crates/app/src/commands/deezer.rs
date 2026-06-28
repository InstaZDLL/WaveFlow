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

use std::path::Path;

use chrono::Utc;
use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use waveflow_core::metadata::{
    deezer::DeezerClient, lastfm::LastfmClient, theaudiodb::TheAudioDbClient,
};

use crate::{
    commands::integration::{
        read_bio_language, read_bio_source, read_lastfm_api_key, BioSource,
    },
    error::{AppError, AppResult},
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
    pub cover_path_1x: Option<String>,
    pub cover_path_2x: Option<String>,
}

impl DeezerAlbumEnrichment {
    fn empty() -> Self {
        Self {
            deezer_id: None,
            label: None,
            release_date: None,
            cover_url: None,
            cover_path: None,
            cover_path_1x: None,
            cover_path_2x: None,
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
    enrich_album_inner(&pool, &artwork_dir, album_id).await
}

pub(crate) async fn enrich_album_inner(
    pool: &SqlitePool,
    artwork_dir: &Path,
    album_id: i64,
) -> AppResult<DeezerAlbumEnrichment> {
    let now = now_ms();

    // 1. Read the local album + its existing deezer_id.
    let local: Option<(String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT al.title, ar.name, al.deezer_id
           FROM album al LEFT JOIN artist ar ON ar.id = al.artist_id
          WHERE al.id = ?",
    )
    .bind(album_id)
    .fetch_optional(pool)
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
               FROM app.metadata_album WHERE deezer_id = ?",
        )
        .bind(did)
        .fetch_optional(pool)
        .await?;

        if let Some((label, release_date, cover_url, cover_hash, expires_at)) = cached {
            if expires_at > now {
                let cover_path = cover_hash
                    .as_deref()
                    .and_then(|h| metadata_artwork::existing_path(artwork_dir, h));
                let (cover_path_1x, cover_path_2x) = match cover_hash.as_deref() {
                    Some(h) => crate::thumbnails::thumbnail_paths_for(artwork_dir, h),
                    None => (None, None),
                };
                return Ok(DeezerAlbumEnrichment {
                    deezer_id: Some(did),
                    label,
                    release_date,
                    cover_url,
                    cover_path,
                    cover_path_1x,
                    cover_path_2x,
                });
            }
        }
    }

    // 3. Fetch from Deezer API — short-circuit when offline mode is
    //    on. Returns whatever Deezer-id we already had (so the UI can
    //    still resolve cached artwork) plus empty enrichment fields.
    if crate::offline::is_offline() {
        return Ok(DeezerAlbumEnrichment {
            deezer_id: existing_deezer_id,
            ..DeezerAlbumEnrichment::empty()
        });
    }
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
        Some(url) => metadata_artwork::download_and_cache(url, artwork_dir).await,
        None => None,
    };
    let cover_path = cover_hash
        .as_deref()
        .and_then(|h| metadata_artwork::existing_path(artwork_dir, h));
    let (cover_path_1x, cover_path_2x) = match cover_hash.as_deref() {
        Some(h) => crate::thumbnails::thumbnail_paths_for(artwork_dir, h),
        None => (None, None),
    };

    // 5. Upsert into cache (now stores the hash too).
    let expires = now + CACHE_TTL_MS;
    sqlx::query(
        "INSERT INTO app.metadata_album
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
    .execute(pool)
    .await?;

    // 6. Link deezer_id on the local album.
    if existing_deezer_id.is_none() {
        sqlx::query("UPDATE album SET deezer_id = ? WHERE id = ?")
            .bind(hit.id)
            .bind(album_id)
            .execute(pool)
            .await?;
    }

    Ok(DeezerAlbumEnrichment {
        deezer_id: Some(hit.id),
        label: hit.label,
        release_date: hit.release_date,
        cover_url,
        cover_path,
        cover_path_1x,
        cover_path_2x,
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
    pub picture_path_1x: Option<String>,
    pub picture_path_2x: Option<String>,
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
            picture_path_1x: None,
            picture_path_2x: None,
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

    // Per-profile bio override (issue #323) wins over any fetched bio
    // and works offline. We still let the inner path fetch + cache the
    // online bio (keeping the shared cross-profile `metadata_artist`
    // cache correct for profiles WITHOUT an override) and just swap the
    // returned bio here. Offline mode short-circuits before the network
    // inside, so the override is the only bio offline users ever see.
    let custom_bio: Option<String> =
        sqlx::query_scalar("SELECT custom_bio FROM artist WHERE id = ?")
            .bind(artist_id)
            .fetch_optional(&pool)
            .await?
            .flatten()
            .map(|b: String| b.trim().to_string())
            .filter(|b| !b.is_empty());

    // Pass the SAME pool into the inner so the custom_bio lookup and the
    // enrichment stay scoped to one profile — re-resolving inside could
    // straddle a switch_profile and apply one profile's override to
    // another's artist.
    let mut enrichment = enrich_artist_deezer_inner(state, pool, artist_id).await?;
    if let Some(bio) = custom_bio {
        enrichment.bio_full = Some(bio.clone());
        enrichment.bio_short = Some(bio);
    }
    Ok(enrichment)
}

async fn enrich_artist_deezer_inner(
    state: tauri::State<'_, AppState>,
    pool: sqlx::SqlitePool,
    artist_id: i64,
) -> AppResult<DeezerArtistEnrichment> {
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

    // Active bio provider + language (issue #295). Read up-front so the
    // cache check can invalidate a bio fetched under a different source
    // / language even when the rest of the row is still fresh.
    let active_source = read_bio_source(&state).await?;
    let active_lang = read_bio_language(&state).await?;

    // 2. Cache hit? (includes bio fields populated in a previous pass)
    if let Some(did) = existing_deezer_id {
        #[allow(clippy::type_complexity)]
        let cached: Option<(
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT picture_url, picture_hash, fans_count, bio_short, bio_full,
                    bio_source, bio_language, expires_at
               FROM app.metadata_artist WHERE deezer_id = ?",
        )
        .bind(did)
        .fetch_optional(&pool)
        .await?;

        if let Some((
            picture_url,
            picture_hash,
            fans_count,
            bio_short,
            bio_full,
            cached_bio_source,
            cached_bio_language,
            expires_at,
        )) = cached
        {
            // The bio part is only reusable when it was fetched under the
            // currently-selected source (and language, for TheAudioDB);
            // otherwise we fall through and re-fetch it.
            let bio_fresh = BioSource::parse(cached_bio_source.as_deref()) == active_source
                && (active_source != BioSource::TheAudioDb
                    || cached_bio_language.as_deref() == Some(active_lang.as_str()));
            if expires_at > now && bio_fresh {
                let picture_path = picture_hash
                    .as_deref()
                    .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
                let (picture_path_1x, picture_path_2x) = match picture_hash.as_deref() {
                    Some(h) => crate::thumbnails::thumbnail_paths_for(&artwork_dir, h),
                    None => (None, None),
                };
                return Ok(DeezerArtistEnrichment {
                    deezer_id: Some(did),
                    picture_url,
                    picture_path,
                    picture_path_1x,
                    picture_path_2x,
                    fans_count,
                    bio_short,
                    bio_full,
                });
            }
        }
    }

    // 3. Fetch from Deezer (picture + fans). Short-circuit when
    //    offline mode is on so we don't poke the network for stale
    //    cache entries.
    if crate::offline::is_offline() {
        return Ok(DeezerArtistEnrichment {
            deezer_id: existing_deezer_id,
            ..DeezerArtistEnrichment::empty()
        });
    }
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

    // 4. Fetch the bio from the selected source (issue #295). Network
    //    failures and missing matches are non-fatal — we still persist
    //    the Deezer portion so the next refresh doesn't spam the
    //    network. The source/language we used is stored alongside so a
    //    later switch re-fetches instead of serving the wrong bio.
    let (bio_short, bio_full) = match active_source {
        BioSource::Lastfm => match read_lastfm_api_key(&state).await? {
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
        },
        BioSource::TheAudioDb => {
            let client = TheAudioDbClient::new();
            match client.artist_bio(&artist_name, &active_lang).await {
                Ok(Some(info)) => (info.bio_short, info.bio_full),
                Ok(None) => (None, None),
                Err(err) => {
                    tracing::warn!(?err, "TheAudioDB artist_bio failed");
                    (None, None)
                }
            }
        }
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
    let (picture_path_1x, picture_path_2x) = match picture_hash.as_deref() {
        Some(h) => crate::thumbnails::thumbnail_paths_for(&artwork_dir, h),
        None => (None, None),
    };

    // 6. Upsert into the metadata cache (Deezer + bio fields land in the
    //    unified `metadata_artist` table in app.db so every profile
    //    shares the same cache). `bio_source` / `bio_language` record
    //    which provider produced the bio so a later switch invalidates
    //    it (see the cache-hit check above). Language is only meaningful
    //    for TheAudioDB, so Last.fm stores NULL.
    let expires = now + CACHE_TTL_MS;
    let stored_lang: Option<&str> =
        matches!(active_source, BioSource::TheAudioDb).then_some(active_lang.as_str());
    sqlx::query(
        "INSERT INTO app.metadata_artist
            (deezer_id, name, picture_url, picture_hash, fans_count, bio_short, bio_full,
             bio_source, bio_language, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deezer_id) DO UPDATE SET
           name = excluded.name,
           picture_url = excluded.picture_url,
           picture_hash = excluded.picture_hash,
           fans_count = excluded.fans_count,
           bio_short = excluded.bio_short,
           bio_full = excluded.bio_full,
           bio_source = excluded.bio_source,
           bio_language = excluded.bio_language,
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
    .bind(active_source.as_str())
    .bind(stored_lang)
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
        picture_path_1x,
        picture_path_2x,
        fans_count: hit.nb_fan,
        bio_short,
        bio_full,
    })
}

// ── Cover management ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeezerAlbumLite {
    pub deezer_id: i64,
    pub title: String,
    pub artist: String,
    pub cover_url: Option<String>,
}

#[tauri::command]
pub async fn search_albums_deezer(query: String) -> AppResult<Vec<DeezerAlbumLite>> {
    if crate::offline::is_offline() {
        return Ok(Vec::new());
    }
    let client = DeezerClient::new();
    let hits = client
        .search_album(&query)
        .await
        .map_err(|err| AppError::Other(format!("deezer search failed: {err}")))?;

    let lite: Vec<DeezerAlbumLite> = hits
        .into_iter()
        .take(20)
        .map(|h| DeezerAlbumLite {
            deezer_id: h.id,
            title: h.title,
            artist: h.artist.map(|a| a.name).unwrap_or_default(),
            cover_url: h.cover_xl.or(h.cover_medium),
        })
        .collect();
    Ok(lite)
}

#[tauri::command]
pub async fn set_album_artwork_from_deezer(
    state: tauri::State<'_, AppState>,
    album_id: i64,
    deezer_album_id: i64,
) -> AppResult<()> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let client = DeezerClient::new();
    let hit = client
        .get_album(deezer_album_id)
        .await
        .map_err(|err| AppError::Other(format!("deezer get_album failed: {err}")))?;

    let cover_url = hit
        .cover_xl
        .clone()
        .or_else(|| hit.cover_big.clone())
        .or_else(|| hit.cover_medium.clone())
        .ok_or_else(|| AppError::Other("deezer album has no cover".into()))?;

    let bytes = download_image_bytes(&cover_url).await?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let format = "jpg";
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    if !target.exists() {
        std::fs::write(&target, &bytes)?;
    }
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    let artwork_id = upsert_artwork_row(&pool, &hash, format, "deezer").await?;
    sqlx::query("UPDATE album SET artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(album_id)
        .execute(&pool)
        .await?;

    Ok(())
}

#[tauri::command]
pub async fn set_album_artwork_from_file(
    state: tauri::State<'_, AppState>,
    album_id: i64,
    file_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let bytes = std::fs::read(&file_path)?;
    let format = detect_image_format(&bytes).ok_or_else(|| {
        AppError::Other("unsupported image format (expected jpg/png/webp)".into())
    })?;

    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    if !target.exists() {
        std::fs::write(&target, &bytes)?;
    }
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    let artwork_id = upsert_artwork_row(&pool, &hash, format, "manual").await?;
    sqlx::query("UPDATE album SET artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(album_id)
        .execute(&pool)
        .await?;

    Ok(())
}

// ── Web Radio now-playing artwork ───────────────────────────────────

/// Resolve cover art for a now-playing Web Radio song. The ICY
/// `StreamTitle` only gives us "Artist - Title" text, so we search
/// Deezer for the track and return its album cover URL. Unlike the
/// library enrichment paths this does NOT cache to disk — a radio
/// now-playing line is ephemeral (changes every song, no library row to
/// link), so a remote CDN URL the `<img>` loads directly is enough.
///
/// Returns `None` when offline, on a network error, or when nothing
/// matched — the frontend keeps the station favicon in that case.
#[tauri::command]
pub async fn fetch_radio_artwork(artist: String, title: String) -> AppResult<Option<String>> {
    if crate::offline::is_offline() {
        return Ok(None);
    }
    let query = format!("{artist} {title}");
    let client = DeezerClient::new();
    let hits = match client.search_track(&query).await {
        Ok(hits) => hits,
        Err(err) => {
            tracing::warn!(?err, "Deezer search_track failed");
            return Ok(None);
        }
    };
    // First hit with an album cover wins — Deezer's relevance ranking
    // already orders the best match first.
    let cover = hits.into_iter().find_map(|h| {
        h.album
            .and_then(|a| a.cover_xl.or(a.cover_big).or(a.cover_medium))
    });
    Ok(cover)
}

// ── Artist image management ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeezerArtistLite {
    pub deezer_id: i64,
    pub name: String,
    pub picture_url: Option<String>,
    pub nb_fan: Option<i64>,
}

/// Search Deezer artists for the artist-image picker. Capped to 20 hits
/// to keep the UI grid readable.
#[tauri::command]
pub async fn search_artists_deezer(query: String) -> AppResult<Vec<DeezerArtistLite>> {
    if crate::offline::is_offline() {
        return Ok(Vec::new());
    }
    let client = DeezerClient::new();
    let hits = client
        .search_artist(&query)
        .await
        .map_err(|err| AppError::Other(format!("deezer artist search failed: {err}")))?;

    Ok(hits
        .into_iter()
        .take(20)
        .map(|h| DeezerArtistLite {
            deezer_id: h.id,
            name: h.name,
            picture_url: h.picture_xl.or(h.picture_big).or(h.picture_medium),
            nb_fan: h.nb_fan,
        })
        .collect())
}

/// Link a specific Deezer artist photo (by Deezer ID) to a local
/// `artist` row. Downloads the picture into the profile artwork cache
/// and overwrites `artist.artwork_id` unconditionally — explicit user
/// pick, so we override any existing image (local sidecar, prior fetch).
#[tauri::command]
pub async fn set_artist_artwork_from_deezer(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
    deezer_artist_id: i64,
) -> AppResult<()> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let client = DeezerClient::new();
    let hit = client
        .get_artist(deezer_artist_id)
        .await
        .map_err(|err| AppError::Other(format!("deezer get_artist failed: {err}")))?;

    let picture_url = hit
        .picture_xl
        .clone()
        .or_else(|| hit.picture_big.clone())
        .or_else(|| hit.picture_medium.clone())
        .ok_or_else(|| AppError::Other("deezer artist has no picture".into()))?;

    let bytes = download_image_bytes(&picture_url).await?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let format = "jpg";
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    if !target.exists() {
        std::fs::write(&target, &bytes)?;
    }
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    let artwork_id = upsert_artwork_row(&pool, &hash, format, "deezer").await?;
    let res = sqlx::query("UPDATE artist SET artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(artist_id)
        .execute(&pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }

    Ok(())
}

/// Manually upload an image file as the artist photo. Same magic-byte
/// validation as `set_album_artwork_from_file` (jpg / png / webp).
#[tauri::command]
pub async fn set_artist_artwork_from_file(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
    file_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let bytes = std::fs::read(&file_path)?;
    let format = detect_image_format(&bytes).ok_or_else(|| {
        AppError::Other("unsupported image format (expected jpg/png/webp)".into())
    })?;

    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    if !target.exists() {
        std::fs::write(&target, &bytes)?;
    }
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    let artwork_id = upsert_artwork_row(&pool, &hash, format, "manual").await?;
    let res = sqlx::query("UPDATE artist SET artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(artist_id)
        .execute(&pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }

    Ok(())
}

/// Detach the current artist image so the resolution chain falls back
/// to the Deezer cache / live fetch. The orphaned `artwork` row (if no
/// longer referenced) is left in place — a future GC pass can sweep it.
#[tauri::command]
pub async fn clear_artist_artwork(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let res = sqlx::query("UPDATE artist SET artwork_id = NULL WHERE id = ?")
        .bind(artist_id)
        .execute(&pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }
    Ok(())
}

/// Walk every artist that doesn't have a fresh `metadata_artist`
/// cache row and run the standard Deezer + Last.fm enrichment on each.
/// Throttles ~5 req/s so the public Deezer API stays happy. Emits
/// `artist-fetch-progress` events the UI can render as a progress bar.
/// Returns the count of artists that ended up with a usable picture.
#[tauri::command]
pub async fn batch_fetch_missing_artist_pictures(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<u32> {
    if crate::offline::is_offline() {
        return Ok(0);
    }
    let pool = state.require_profile_pool().await?;
    let now = now_ms();

    // Pending = artists whose cached row is missing OR expired. The
    // LEFT JOIN on a.deezer_id covers both "never linked yet" and
    // "linked but cache expired" in one pass.
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT a.id, a.name
           FROM artist a
           LEFT JOIN app.metadata_artist m ON m.deezer_id = a.deezer_id
          WHERE a.deezer_id IS NULL
             OR m.expires_at IS NULL
             OR m.expires_at <= ?
          ORDER BY a.name COLLATE NOCASE",
    )
    .bind(now)
    .fetch_all(&pool)
    .await?;

    let total = rows.len();
    let mut success: u32 = 0;
    for (i, (artist_id, name)) in rows.into_iter().enumerate() {
        let _ = app.emit(
            "artist-fetch-progress",
            serde_json::json!({
                "current": i + 1,
                "total": total,
                "artist_name": name,
            }),
        );
        match enrich_artist_deezer(state.clone(), artist_id).await {
            Ok(e) => {
                if e.picture_path.is_some() {
                    success += 1;
                }
            }
            Err(err) => {
                tracing::warn!(artist_id, ?err, "batch artist fetch failed");
            }
        }
        // Throttle to be polite — Deezer's anonymous API tolerates
        // ~50 req/5s; 200 ms keeps us comfortably under the limit and
        // lets a user open another tab without lag.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    Ok(success)
}

#[tauri::command]
pub async fn batch_fetch_missing_album_covers(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<u32> {
    if crate::offline::is_offline() {
        return Ok(0);
    }
    let pool = state.require_profile_pool().await?;
    let artwork_dir = state.paths.metadata_artwork_dir.clone();

    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT a.id, a.title FROM album a WHERE a.artwork_id IS NULL")
            .fetch_all(&pool)
            .await?;

    let total = rows.len();
    // Emit an initial frame so the UI can show the total — and explicitly
    // surface the "nothing to do" case (total == 0) which otherwise looks
    // like the button does nothing.
    let _ = app.emit(
        "cover-fetch-progress",
        serde_json::json!({
            "current": 0,
            "total": total,
            "album_title": "",
        }),
    );
    let mut success: u32 = 0;
    for (i, (album_id, title)) in rows.into_iter().enumerate() {
        let _ = app.emit(
            "cover-fetch-progress",
            serde_json::json!({
                "current": i + 1,
                "total": total,
                "album_title": title,
            }),
        );
        match enrich_album_inner(&pool, &artwork_dir, album_id).await {
            Ok(enrich) => {
                if enrich.cover_path.is_some() {
                    success += 1;
                }
            }
            Err(err) => {
                tracing::warn!(album_id, ?err, "batch cover fetch failed");
            }
        }
    }

    Ok(success)
}

async fn upsert_artwork_row(
    pool: &SqlitePool,
    hash: &str,
    format: &str,
    source: &str,
) -> AppResult<i64> {
    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM artwork WHERE hash = ?")
        .bind(hash)
        .fetch_optional(pool)
        .await?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let result =
        sqlx::query("INSERT INTO artwork (hash, format, source, created_at) VALUES (?, ?, ?, ?)")
            .bind(hash)
            .bind(format)
            .bind(source)
            .bind(now_ms())
            .execute(pool)
            .await?;
    Ok(result.last_insert_rowid())
}

fn detect_image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("jpg");
    }
    if bytes.len() >= 8
        && bytes[0] == 0x89
        && bytes[1] == 0x50
        && bytes[2] == 0x4E
        && bytes[3] == 0x47
        && bytes[4] == 0x0D
        && bytes[5] == 0x0A
        && bytes[6] == 0x1A
        && bytes[7] == 0x0A
    {
        return Some("png");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

/// Hard cap on a single image download. Deezer's `picture_xl` /
/// `cover_xl` payloads top out around 200 KB; 10 MiB is generous
/// headroom while still guarding against a hostile (or compromised)
/// remote that streams unbounded data into our process memory.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

async fn download_image_bytes(url: &str) -> AppResult<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent("WaveFlow/0.1")
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|err| AppError::Other(format!("http client build failed: {err}")))?;

    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|err| AppError::Other(format!("download failed: {err}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(format!(
            "download status {}",
            resp.status()
        )));
    }

    // Early-reject when the server is honest about its size — saves us
    // pulling a single chunk we'd throw away anyway.
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_IMAGE_BYTES {
            return Err(AppError::Other(format!(
                "image too large ({} bytes, max {})",
                len, MAX_IMAGE_BYTES,
            )));
        }
    }

    // Pull chunk-by-chunk via `Response::chunk()` (built-in, no need
    // for the `stream` feature on reqwest) so a server lying about
    // Content-Length — or chunked transfer with no length at all —
    // still can't OOM us.
    let mut bytes = Vec::with_capacity(64 * 1024);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|err| AppError::Other(format!("read failed: {err}")))?
    {
        if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
            return Err(AppError::Other(format!(
                "image exceeds max size ({} bytes)",
                MAX_IMAGE_BYTES,
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
