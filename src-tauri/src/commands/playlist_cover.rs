//! Cover image management for user playlists.
//!
//! Three flavours, all writing to `playlist.cover_hash`:
//! - **Manual upload** — user picks a file. We magic-byte validate, blake3
//!   the bytes, save to the shared `metadata_artwork` cache, set
//!   `cover_is_auto = 0` so the auto-regen path stops touching the row.
//! - **Auto-regenerate** — composite the first 4 tracks' album artworks
//!   into a 2×2 grid à la Spotify. Triggered explicitly (this command) or
//!   implicitly via [`maybe_regen_auto_cover`] after every mutation that
//!   could change the first-4 set.
//! - **Clear** — drop the cover_hash and flip back to auto, then
//!   immediately regenerate so the visual feedback is instant rather than
//!   "stays empty until the next mutation".
//!
//! Smart playlists (`is_smart = 1`) are excluded from every code path here:
//! their covers are owned by the smart-playlist regen flow.

use std::path::PathBuf;

use sqlx::{FromRow, SqlitePool};

use crate::error::{AppError, AppResult};
use crate::paths::AppPaths;
use crate::smart_playlists::cover;
use crate::state::AppState;

/// Magic-byte validator copied from the album-cover upload path. Kept here
/// to avoid a cross-module import on a 20-line helper that the two callers
/// can comfortably duplicate.
fn detect_image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("jpg");
    }
    if bytes.len() >= 8
        && bytes[0] == 0x89
        && bytes[1] == 0x50
        && bytes[2] == 0x4E
        && bytes[3] == 0x47
    {
        return Some("png");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

/// Hard cap on a single uploaded image. Big enough for an unprocessed 4K
/// JPEG (~5 MB) plus headroom; small enough to stop a rogue file from
/// pulling the whole desktop into RAM.
const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Number of tiles in the auto-cover grid. Mirrors the 2×2 layout in
/// [`cover::build_composite_cover`] — anything below 4 falls through to a
/// strips layout, which still looks fine but isn't the Spotify look.
const AUTO_COVER_TILE_COUNT: usize = 4;

#[derive(FromRow)]
struct PlaylistFlags {
    is_smart: i64,
    cover_is_auto: i64,
}

/// Upload a user-supplied image as the playlist cover. Sets
/// `cover_is_auto = 0` so the auto-regen path stops overwriting it. Same
/// magic-byte validation + blake3 dedup as the album-cover upload command.
#[tauri::command]
pub async fn set_playlist_cover_from_file(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    file_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    ensure_user_playlist(&pool, playlist_id).await?;

    let bytes =
        std::fs::read(&file_path).map_err(|e| AppError::Other(format!("read upload: {e}")))?;
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(AppError::Other(format!(
            "image too large: {} bytes (max {})",
            bytes.len(),
            MAX_UPLOAD_BYTES
        )));
    }
    if detect_image_format(&bytes).is_none() {
        return Err(AppError::Other(
            "unsupported image format (expected jpg/png/webp)".into(),
        ));
    }
    // We don't keep the original extension in the metadata cache because
    // every smart-playlist cover is JPEG. Re-encode through the compositor
    // pipeline (single-tile fill) for two wins:
    //   1. Format normalisation — every `cover_hash` resolves to .jpg, no
    //      branching on `playlist_artwork_hash + extension`.
    //   2. Resize-to-canvas — uploads larger than 640×640 get downscaled
    //      to the on-disk standard, capping disk usage and keeping the
    //      sidebar / carousel render fast.
    let tmp = std::env::temp_dir().join(format!(
        "waveflow-upload-{}.bin",
        blake3::hash(&bytes).to_hex()
    ));
    std::fs::write(&tmp, &bytes).map_err(|e| AppError::Other(format!("temp write: {e}")))?;
    let result = cover::build_composite_cover(
        std::slice::from_ref(&tmp),
        &state.paths.metadata_artwork_dir,
    );
    let _ = std::fs::remove_file(&tmp);
    let hash = result?;

    update_cover(&pool, playlist_id, Some(&hash), 0).await?;
    Ok(())
}

/// Force a regen of the auto-cover for `playlist_id`. The Spotify modal
/// invokes this implicitly through [`clear_playlist_cover`]; we expose it
/// as its own command in case the frontend ever needs an explicit refresh
/// button.
#[tauri::command]
pub async fn regenerate_playlist_auto_cover(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    ensure_user_playlist(&pool, playlist_id).await?;
    regenerate_inner(&pool, &state.paths, profile_id, playlist_id).await
}

/// Drop the manual cover and switch back to auto. Immediately re-runs the
/// auto-cover so the user sees a fresh composite instead of falling back
/// to the gradient tile until the next mutation.
#[tauri::command]
pub async fn clear_playlist_cover(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    ensure_user_playlist(&pool, playlist_id).await?;
    update_cover(&pool, playlist_id, None, 1).await?;
    // Best-effort regen — a brand new playlist with < 4 tracks legitimately
    // has nothing to compose yet, so a "nothing to do" return is fine.
    let _ = regenerate_inner(&pool, &state.paths, profile_id, playlist_id).await;
    Ok(())
}

/// Hook called by playlist-mutation commands (add/remove/reorder/source).
/// No-op when the playlist is smart, the user has uploaded their own cover
/// (`cover_is_auto = 0`), or there aren't enough tracks yet for a proper
/// 2×2 composite.
pub async fn maybe_regen_auto_cover(
    pool: &SqlitePool,
    paths: &AppPaths,
    profile_id: i64,
    playlist_id: i64,
) {
    let flags: Option<PlaylistFlags> =
        sqlx::query_as("SELECT is_smart, cover_is_auto FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let Some(flags) = flags else { return };
    if flags.is_smart == 1 || flags.cover_is_auto == 0 {
        return;
    }
    if let Err(err) = regenerate_inner(pool, paths, profile_id, playlist_id).await {
        // Auto-regen failures are never fatal to the mutation that
        // triggered them — the user still gets their add/remove/reorder
        // outcome, just without the cover refresh.
        tracing::warn!(playlist_id, ?err, "auto-cover regen failed (non-fatal)");
    }
}

async fn regenerate_inner(
    pool: &SqlitePool,
    paths: &AppPaths,
    profile_id: i64,
    playlist_id: i64,
) -> AppResult<()> {
    let paths_for_compose =
        top_track_artwork_paths(pool, paths, profile_id, playlist_id, AUTO_COVER_TILE_COUNT).await;
    if paths_for_compose.is_empty() {
        // Nothing to compose — keep whatever cover is already there
        // (typically NULL for a brand new playlist). The frontend's
        // gradient fallback handles display.
        tracing::info!(
            playlist_id,
            "auto-cover: no usable album artworks, keeping existing cover_hash"
        );
        return Ok(());
    }
    let hash = cover::build_composite_cover(&paths_for_compose, &paths.metadata_artwork_dir)?;
    update_cover(pool, playlist_id, Some(&hash), 1).await?;
    tracing::info!(playlist_id, %hash, tile_count = paths_for_compose.len(), "auto-cover regenerated");
    Ok(())
}

/// Pull artwork file paths for the first `take` tracks of the playlist
/// (in `position` order) that have an album cover in the per-profile
/// cache. Tracks without artwork are silently skipped — we walk in order
/// until `take` is hit or the source is exhausted.
async fn top_track_artwork_paths(
    pool: &SqlitePool,
    paths: &AppPaths,
    profile_id: i64,
    playlist_id: i64,
    take: usize,
) -> Vec<PathBuf> {
    #[derive(FromRow)]
    struct Row {
        hash: String,
        format: String,
    }
    // Pull more than `take` so we can keep walking past tracks whose
    // artwork file vanished from disk (cache wipe, manual delete) without
    // a second round-trip.
    let limit = (take * 4) as i64;
    let rows: Vec<Row> = sqlx::query_as(
        r#"
        SELECT aw.hash   AS hash,
               aw.format AS format
          FROM playlist_track pt
          JOIN track t       ON t.id  = pt.track_id
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE pt.playlist_id = ?
           AND t.is_available = 1
           AND aw.hash IS NOT NULL
         ORDER BY pt.position ASC
         LIMIT ?
        "#,
    )
    .bind(playlist_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let artwork_dir = paths.profile_artwork_dir(profile_id);
    let mut out = Vec::with_capacity(take);
    for row in rows {
        if out.len() >= take {
            break;
        }
        let p = artwork_dir.join(format!("{}.{}", row.hash, row.format));
        if p.exists() {
            out.push(p);
        }
    }
    out
}

/// Reject smart playlists and missing ids with a precise error. Keeps every
/// public command above honest about what they accept.
async fn ensure_user_playlist(pool: &SqlitePool, playlist_id: i64) -> AppResult<()> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT is_smart FROM playlist WHERE id = ?")
        .bind(playlist_id)
        .fetch_optional(pool)
        .await?;
    match row {
        None => Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        ))),
        Some((1,)) => Err(AppError::Other(
            "smart playlists manage their own cover — not editable here".into(),
        )),
        Some((_,)) => Ok(()),
    }
}

async fn update_cover(
    pool: &SqlitePool,
    playlist_id: i64,
    cover_hash: Option<&str>,
    cover_is_auto: i64,
) -> AppResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE playlist
            SET cover_hash    = ?,
                cover_is_auto = ?,
                updated_at    = ?
          WHERE id = ?",
    )
    .bind(cover_hash)
    .bind(cover_is_auto)
    .bind(now)
    .bind(playlist_id)
    .execute(pool)
    .await?;
    Ok(())
}
