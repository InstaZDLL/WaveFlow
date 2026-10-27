//! Choosing a cover or an artist picture by hand: the Deezer searches,
//! a local file, and the cover of a radio track.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use waveflow_core::artwork::image_format::detect_image_format;
use waveflow_core::metadata::deezer::DeezerClient;
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
    let (pool, profile_id) = state.require_profile_snapshot().await?;
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
    waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    // One transaction so the artwork row and the album link land
    // together — see `upsert_artwork`'s contract in CLAUDE.md.
    let mut tx = pool.begin().await?;
    let artwork_id = upsert_artwork(&mut tx, &hash, format, ArtworkSource::Deezer).await?;
    let res =
        sqlx::query("UPDATE album SET artwork_id = ?, artwork_source = 'deezer' WHERE id = ?")
            .bind(artwork_id)
            .bind(album_id)
            .execute(&mut *tx)
            .await?;
    // Matching no row means the album is gone (stale UI, concurrent
    // delete). Returning early leaves `tx` un-committed, so the artwork
    // insert rolls back instead of landing with nothing pointing at it —
    // and the caller hears about it rather than getting a silent success.
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("album {album_id} not found")));
    }
    tx.commit().await?;

    Ok(())
}

#[tauri::command]
pub async fn set_album_artwork_from_file(
    state: tauri::State<'_, AppState>,
    album_id: i64,
    file_path: String,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let bytes = std::fs::read(&file_path)?;
    let format = detect_image_format(&bytes).ok_or_else(|| {
        AppError::Other("unsupported image format (expected jpg/png/webp)".into())
    })?;

    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    // One transaction so the artwork row and the album link land
    // together — see `upsert_artwork`'s contract in CLAUDE.md.
    let mut tx = pool.begin().await?;
    let artwork_id = upsert_artwork(&mut tx, &hash, format, ArtworkSource::Manual).await?;
    let res =
        sqlx::query("UPDATE album SET artwork_id = ?, artwork_source = 'manual' WHERE id = ?")
            .bind(artwork_id)
            .bind(album_id)
            .execute(&mut *tx)
            .await?;
    // Matching no row means the album is gone (stale UI, concurrent
    // delete). Returning early leaves `tx` un-committed, so the artwork
    // insert rolls back instead of landing with nothing pointing at it —
    // and the caller hears about it rather than getting a silent success.
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("album {album_id} not found")));
    }
    tx.commit().await?;

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
        .map(|h| {
            // Skip Deezer's empty-hash placeholder (#406) — borrow before
            // `h.name` moves into the struct.
            let picture_url = h.best_picture();
            DeezerArtistLite {
                deezer_id: h.id,
                name: h.name,
                picture_url,
                nb_fan: h.nb_fan,
            }
        })
        .collect())
}

/// Link a specific Deezer artist to a local `artist` row: its photo
/// **and its identity**. Downloads the picture into the profile artwork
/// cache and overwrites `artist.artwork_id` unconditionally — explicit
/// user pick, so we override any existing image (local sidecar, prior
/// fetch).
///
/// It also writes `artist.deezer_id` (#692). The picker is the one place
/// where a user sees the candidates side by side with their fan counts,
/// so picking one is an answer to "which artist is this?", not only
/// "which photo?". Everything else resolves through that column — fan
/// count, bio, TheAudioDB hero fanart, similar artists — so writing the
/// photo alone left the page showing one artist's picture above another
/// artist's numbers.
///
/// Nothing overwrites a non-NULL `deezer_id` afterwards: the automatic
/// link in `enrich_artist_deezer_inner` only fills the column when it is
/// empty, so the hand-picked id survives every later enrichment pass.
/// `set_artist_artwork_from_file` deliberately does *not* do this — a
/// local file says nothing about which Deezer artist this is.
#[tauri::command]
pub async fn set_artist_artwork_from_deezer(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
    deezer_artist_id: i64,
) -> AppResult<()> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let client = DeezerClient::new();
    let hit = client
        .get_artist(deezer_artist_id)
        .await
        .map_err(|err| AppError::Other(format!("deezer get_artist failed: {err}")))?;

    let picture_url = hit
        .best_picture()
        .ok_or_else(|| AppError::Other("deezer artist has no picture".into()))?;

    let bytes = download_image_bytes(&picture_url).await?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let format = "jpg";
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    // Same transaction discipline as the album paths above. The
    // early return below leaves `tx` un-committed, so a missing artist
    // rolls the artwork insert back rather than orphaning it.
    let mut tx = pool.begin().await?;
    let artwork_id = upsert_artwork(&mut tx, &hash, format, ArtworkSource::Deezer).await?;
    // Photo and identity in one statement: a row that ends up with the
    // new picture and the old id is the split this fixes.
    let res = sqlx::query("UPDATE artist SET artwork_id = ?, deezer_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(deezer_artist_id)
        .bind(artist_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }
    tx.commit().await?;

    // The enrichment cache is keyed by deezer_id, so the row cached
    // under the previous id is simply no longer reached — nothing to
    // invalidate. What does need telling is every view holding a
    // resolved photo for this artist; they cache per artist id and have
    // no other reason to look again.
    let _ = app.emit("artist:updated", artist_id);

    Ok(())
}

/// Manually upload an image file as the artist photo. Same magic-byte
/// validation as `set_album_artwork_from_file` (jpg / png / webp).
#[tauri::command]
pub async fn set_artist_artwork_from_file(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
    file_path: String,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&profile_artwork_dir)?;

    let bytes = std::fs::read(&file_path)?;
    let format = detect_image_format(&bytes).ok_or_else(|| {
        AppError::Other("unsupported image format (expected jpg/png/webp)".into())
    })?;

    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    // Same transaction discipline as the album paths above. The
    // early return below leaves `tx` un-committed, so a missing artist
    // rolls the artwork insert back rather than orphaning it.
    let mut tx = pool.begin().await?;
    let artwork_id = upsert_artwork(&mut tx, &hash, format, ArtworkSource::Manual).await?;
    let res = sqlx::query("UPDATE artist SET artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(artist_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }
    tx.commit().await?;
    let _ = app.emit("artist:updated", artist_id);

    Ok(())
}

/// Detach the current artist image so the resolution chain falls back
/// to the Deezer cache / live fetch. The orphaned `artwork` row (if no
/// longer referenced) is left in place — a future GC pass can sweep it.
#[tauri::command]
pub async fn clear_artist_artwork(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let res = sqlx::query("UPDATE artist SET artwork_id = NULL WHERE id = ?")
        .bind(artist_id)
        .execute(&*pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("artist {artist_id} not found")));
    }
    let _ = app.emit("artist:updated", artist_id);
    Ok(())
}
