//! Replacing a track's embedded cover.

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use waveflow_core::scanner::{upsert_artwork, ArtworkSource};

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

/// Replace the embedded cover for a track. The new image is written
/// into the audio file's tag (replacing the front cover, and only the
/// front cover — a release that ships a booklet or a back cover keeps
/// them) AND copied into the per-profile artwork cache, then the track's
/// album.artwork_id is repointed at the new row. Cover is per-album
/// in WaveFlow's data model, so editing one track repaints every
/// sibling on the same album — matching the behaviour every other
/// music player ships.
#[tauri::command]
pub async fn update_track_cover(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    app: AppHandle,
    track_id: i64,
    image_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&artwork_dir)?;

    let row: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT file_path, album_id FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
    let (file_path, album_id) =
        row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;

    let bytes = std::fs::read(&image_path)
        .map_err(|e| AppError::Other(format!("cover read failed: {e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Other("cover file is empty".into()));
    }
    let (mime, ext) = sniff_image_mime(&bytes, &image_path);

    // Pause if the engine has the file open — same reason as the
    // tag-edit path: the write opens the real file.
    let active = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    if active == track_id {
        let _ = engine.send(crate::audio::AudioCmd::Pause);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // A cover is the largest thing we ever push into a tag, so this is
    // the write least suited to an async worker.
    {
        let path = std::path::PathBuf::from(&file_path);
        let bytes = bytes.clone();
        let mime = mime.clone();
        tokio::task::spawn_blocking(move || write_cover_to_file(&path, &bytes, &mime))
            .await
            .map_err(|e| AppError::Other(format!("cover tag write join: {e}")))?
            .map_err(|e| AppError::Other(format!("cover tag write failed: {e}")))?;
    }

    // The audio file itself just changed (a new picture frame was
    // embedded), so its blake3 hash drifted. Recompute and persist so
    // the scanner's fast-path and the lyrics cache (keyed on file_hash)
    // stay valid. Propagate the error — leaving `track.file_hash`
    // pointing at the pre-write digest would silently invalidate the
    // lyrics cache for this row and confuse the next scan pass.
    rehash_track_file(&pool, track_id, std::path::Path::new(&file_path)).await?;

    // Hash + persist the bytes in the shared artwork cache. blake3
    // makes "same image, different file" deduplicate naturally.
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let out_path = artwork_dir.join(format!("{hash}.{ext}"));
    waveflow_core::atomic_write::store_content_addressed(&out_path, &bytes)
        .map_err(|e| AppError::Other(format!("artwork cache write failed: {e}")))?;
    crate::thumbnails::spawn_thumbnail_job(out_path.clone(), artwork_dir.clone(), hash.clone());

    // One transaction rather than two pooled connections: the artwork
    // row and the album link belong together, and the previous shape
    // could leave the row behind if the update failed.
    let mut tx = pool.begin().await?;
    let artwork_id = upsert_artwork(&mut tx, &hash, ext, ArtworkSource::Manual).await?;
    // Only guarded when a track actually belongs to an album: `album_id`
    // is legitimately `None` for a loose track, and that case must still
    // commit — the cover has already been written into the audio file
    // itself, which is the point of this command.
    if let Some(aid) = album_id {
        let res =
            sqlx::query("UPDATE album SET artwork_id = ?, artwork_source = 'manual' WHERE id = ?")
                .bind(artwork_id)
                .bind(aid)
                .execute(&mut *tx)
                .await?;
        if res.rows_affected() == 0 {
            return Err(AppError::Other(format!("album {aid} not found")));
        }
    }
    tx.commit().await?;

    let _ = app.emit("track:updated", track_id);
    let _ = app.emit("library:rescanned", ());
    let _ = app.emit("player:queue-changed", ());
    Ok(())
}

/// Write `bytes` as the file's front cover, leaving every other
/// embedded picture (back cover, booklet, artist shot) in place.
fn write_cover_to_file(
    path: &std::path::Path,
    bytes: &[u8],
    mime: &lofty::picture::MimeType,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    patch_file(path, &TagPatch::Cover { bytes, mime })
}

/// Pick the MIME type + filename extension for the user-supplied
/// image. Magic-byte first, fall back to the path extension when the
/// header is unrecognised. Lofty stores WebP under `Unknown` because
/// the enum doesn't have a first-class variant for it.
fn sniff_image_mime(bytes: &[u8], path: &str) -> (lofty::picture::MimeType, &'static str) {
    use lofty::picture::MimeType;
    if bytes.len() >= 4 {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return (MimeType::Jpeg, "jpg");
        }
        if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            return (MimeType::Png, "png");
        }
        if bytes.starts_with(b"GIF8") {
            return (MimeType::Gif, "gif");
        }
        if bytes.starts_with(&[0x42, 0x4D]) {
            return (MimeType::Bmp, "bmp");
        }
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return (MimeType::Unknown("image/webp".into()), "webp");
        }
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        (MimeType::Jpeg, "jpg")
    } else if lower.ends_with(".png") {
        (MimeType::Png, "png")
    } else if lower.ends_with(".webp") {
        (MimeType::Unknown("image/webp".into()), "webp")
    } else {
        (MimeType::Jpeg, "jpg")
    }
}
