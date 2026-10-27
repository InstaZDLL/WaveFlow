//! The artist background: its candidates, setting it from a URL or a
//! file, and clearing it.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use waveflow_core::artwork::image_format::detect_image_format;
// Shared with the scanner: takes a connection so the caller controls
// the transaction (see CLAUDE.md, "Single writer to SQLite"). This
// module used to carry a pool-taking copy of it, which is what kept
// these paths non-transactional.
use waveflow_core::scanner::{upsert_artwork, ArtworkSource};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

// ── Artist backdrop (the wide hero image) ───────────────────────────

/// The wide backdrops on offer for an artist: every fanart TheAudioDB
/// listed for it (issue #693), cached in `app.metadata_artist` under
/// the artist's `deezer_id`.
///
/// Empty means the artist has none — or that nothing has looked yet,
/// which the artist page fixes by running its usual enrichment first.
/// The URLs are remote and are what the picker shows as thumbnails;
/// only the one the user settles on is downloaded.
#[tauri::command]
pub async fn get_artist_backdrop_candidates(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<ArtistBackdropCandidates> {
    // The candidates are remote URLs, and the picker paints them as
    // thumbnails — so handing them over in offline mode makes the
    // webview fetch a dozen images from TheAudioDB's CDN, which is
    // exactly what offline mode promises not to do. `offline` rides
    // along so the picker can say that rather than claim the artist has
    // none: two calls could straddle a change of the setting and pair
    // an empty list with "we are online".
    if crate::offline::is_offline() {
        return Ok(ArtistBackdropCandidates {
            urls: Vec::new(),
            offline: true,
        });
    }
    let pool = state.require_profile_pool().await?;
    Ok(ArtistBackdropCandidates {
        urls: read_backdrop_candidates(&pool, artist_id).await?,
        offline: false,
    })
}

/// What the backdrop picker needs to fill its first tab: the candidates
/// and the reason there may be none.
#[derive(Debug, Clone, Serialize)]
pub struct ArtistBackdropCandidates {
    pub urls: Vec<String>,
    /// `true` when the list is empty because offline mode refused it,
    /// rather than because the artist has no fanart.
    pub offline: bool,
}

/// The cached candidate list for one artist. Shared by the command that
/// offers it and the one that accepts a pick, so the set the user can
/// choose from and the set the backend will fetch are the same set by
/// construction.
async fn read_backdrop_candidates(
    pool: &sqlx::SqlitePool,
    artist_id: i64,
) -> AppResult<Vec<String>> {
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT m.background_urls
           FROM artist a
           JOIN app.metadata_artist m ON m.deezer_id = a.deezer_id
          WHERE a.id = ?",
    )
    .bind(artist_id)
    .fetch_optional(pool)
    .await?
    .flatten();

    Ok(stored
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default())
}

/// Download one of this artist's candidate backdrops and make it the
/// chosen one.
///
/// `url` **must be one of the candidates** `get_artist_backdrop_candidates`
/// returned for this artist. The check is not about the picker, which
/// only ever passes one of them: a command is an API, and without it
/// this one fetches any URL the caller names, from a process that sits
/// inside the user's network. Checking against the cached list keeps
/// the reachable set to what TheAudioDB itself published.
///
/// The bytes land in the **profile** artwork dir rather than the shared
/// metadata cache, like every other manual pick: a cache prune sweeps
/// what no `metadata_artist` row references, and a chosen backdrop must
/// survive that. Full resolution, no thumbnail tiers — the hero paints
/// it full-bleed, so a downscaled copy would only soften it.
#[tauri::command]
pub async fn set_artist_background_from_url(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
    url: String,
) -> AppResult<()> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let candidates = read_backdrop_candidates(&pool, artist_id).await?;
    if !candidates.iter().any(|c| c == &url) {
        return Err(AppError::Other(
            "not one of this artist's backdrop candidates".into(),
        ));
    }
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let bytes = download_image_bytes(&url).await?;
    let format = detect_image_format(&bytes)
        .ok_or_else(|| AppError::Other("unsupported image format".into()))?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;

    // A suggestion the user picked is a manual pick, whichever provider
    // offered it. This wrote "theaudiodb", a value `artwork.source`'s
    // CHECK constraint refuses, so every suggestion failed (#750).
    set_artist_background_artwork(&pool, artist_id, &hash, format, ArtworkSource::Manual).await?;
    let _ = app.emit("artist:updated", artist_id);
    Ok(())
}

/// Use a local image file as the artist backdrop. Same magic-byte
/// validation as the photo picker (jpg / png / webp).
#[tauri::command]
pub async fn set_artist_background_from_file(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
    file_path: String,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    // Checked before the read, not after: a hero is painted full-bleed
    // so it is the one image with a reason to be large, but the file is
    // named by the user and nothing else bounds it. The downloader above
    // caps its own stream at the same size.
    let size = std::fs::metadata(&file_path)?.len();
    if size > MAX_IMAGE_BYTES as u64 {
        return Err(AppError::Other(format!(
            "image too large ({size} bytes, max {MAX_IMAGE_BYTES})"
        )));
    }
    let bytes = std::fs::read(&file_path)?;
    let format = detect_image_format(&bytes).ok_or_else(|| {
        AppError::Other("unsupported image format (expected jpg/png/webp)".into())
    })?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;

    set_artist_background_artwork(&pool, artist_id, &hash, format, ArtworkSource::Manual).await?;
    let _ = app.emit("artist:updated", artist_id);
    Ok(())
}

/// Drop the chosen backdrop so the hero falls back to the automatic
/// one: TheAudioDB's first fanart, or the blurred square photo.
#[tauri::command]
pub async fn clear_artist_background(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let res = sqlx::query("UPDATE artist SET background_artwork_id = NULL WHERE id = ?")
        .bind(artist_id)
        .execute(&*pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }
    let _ = app.emit("artist:updated", artist_id);
    Ok(())
}

/// Shared tail of the two backdrop setters: register the artwork row and
/// point the artist at it, in one transaction so a missing artist rolls
/// the artwork insert back rather than orphaning it — the discipline the
/// photo setters above follow.
async fn set_artist_background_artwork(
    pool: &sqlx::SqlitePool,
    artist_id: i64,
    hash: &str,
    format: &str,
    source: ArtworkSource,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let artwork_id = upsert_artwork(&mut tx, hash, format, source).await?;
    let res = sqlx::query("UPDATE artist SET background_artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(artist_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }
    tx.commit().await?;
    Ok(())
}
