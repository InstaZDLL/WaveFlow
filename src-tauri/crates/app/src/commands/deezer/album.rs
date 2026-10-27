//! Enriching an album from Deezer: its cover, its identifier and the cache
//! row behind them.

use std::path::Path;

use serde::Serialize;
use sqlx::SqlitePool;

use waveflow_core::metadata::deezer::DeezerClient;

use crate::{error::AppResult, metadata_artwork, state::AppState};

use super::*;

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

/// Whether a fresh cached `metadata_album` row is a usable hit **for the needs
/// of the album being enriched**. A row with no `cover_hash` still counts as
/// complete when the local album has its own artwork (it will never need the
/// Deezer cover). But for an art-less album a cover-less row is *incomplete* —
/// it must trigger a re-fetch instead of serving a permanent miss. Without this
/// the #493 download-skip would poison the shared cache: a fresh cover-less row
/// (art removed, a different profile sharing the cache, or a prior failed
/// download) would block the cover from ever being fetched until the TTL lapsed.
pub(super) fn metadata_album_cache_complete(cover_hash: Option<&str>, has_local_art: bool) -> bool {
    cover_hash.is_some() || has_local_art
}

pub(crate) async fn enrich_album_inner(
    pool: &SqlitePool,
    artwork_dir: &Path,
    album_id: i64,
) -> AppResult<DeezerAlbumEnrichment> {
    let now = now_ms();

    // 1. Read the local album + its existing deezer_id + whether it already
    //    has local artwork (issue #493 — see the cover-download guard below).
    let local: Option<(String, Option<String>, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT al.title, ar.name, al.deezer_id, al.artwork_id
           FROM album al LEFT JOIN artist ar ON ar.id = al.artist_id
          WHERE al.id = ?",
    )
    .bind(album_id)
    .fetch_optional(pool)
    .await?;

    let Some((album_title, artist_name, existing_deezer_id, local_artwork_id)) = local else {
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
            // A fresh row is a usable hit only when it's also complete for this
            // album's needs — otherwise a cover-less row for an art-less album
            // would block a re-fetch until the TTL lapsed (issue #493).
            let usable = expires_at > now
                && metadata_album_cache_complete(cover_hash.as_deref(), local_artwork_id.is_some());
            if usable {
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

    // 4. Download artwork into the shared cache (best-effort) — but ONLY for an
    //    album that has NO local cover of its own (issue #493). This function is
    //    fired automatically every time an album page opens (which only reads
    //    `label` + `release_date`) and by the Discord presence (which reads the
    //    remote `cover_url`); neither uses the downloaded file, and the album
    //    grid / detail header render the LOCAL artwork. Without this guard the
    //    shared `metadata_artwork` cache filled up with Deezer covers for albums
    //    the user already has artwork for — never displayed. The deliberate
    //    paths still work: `batch_fetch_missing_album_covers` only iterates
    //    `artwork_id IS NULL` albums, and a genuinely cover-less album still
    //    gets its fallback. `cover_url` always rides through for Discord + the
    //    cache row regardless.
    let cover_hash = match (local_artwork_id.is_none(), cover_url.as_deref()) {
        (true, Some(url)) => metadata_artwork::download_and_cache(url, artwork_dir).await,
        _ => None,
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
           -- Only overwrite the cached hash on a NEW successful download.
           -- `excluded.cover_hash` is NULL when the download was skipped
           -- (art-having album, #493) or failed transiently — in both cases
           -- keep whatever cover was already cached rather than dropping a good
           -- one over a network blip. The #493 cleanup is what deliberately
           -- clears art-having albums' covers, not this best-effort upsert.
           cover_hash = COALESCE(excluded.cover_hash, cover_hash),
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
