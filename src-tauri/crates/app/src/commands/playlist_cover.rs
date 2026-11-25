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
//! A custom smart playlist (rules the user wrote) takes all three, like a
//! user playlist: its auto-cover is regenerated after its rules run and
//! change its tracks ([`regen_auto_covers`]). The generated families
//! (Daily Mix, On Repeat, Release Radar, the themed mixes) are refused by
//! every command here: their generator writes their cover (#1173).

use base64::Engine;
use std::io::Read;
use std::path::PathBuf;

use sqlx::{FromRow, SqlitePool};
use waveflow_core::artwork::image_format::detect_image_format;

use crate::error::{AppError, AppResult};
use crate::paths::AppPaths;
use crate::smart_playlists::cover;
use crate::state::AppState;

/// Hard cap on a single uploaded image. Big enough for an unprocessed 4K
/// JPEG (~5 MB) plus headroom; small enough to stop a rogue file from
/// pulling the whole desktop into RAM.
const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

fn read_cover_bytes(file_path: &str) -> AppResult<Vec<u8>> {
    let file = std::fs::File::open(file_path)
        .map_err(|e| AppError::Other(format!("open cover image: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_UPLOAD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::Other(format!("read cover image: {e}")))?;
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(AppError::shown(
            crate::error::ShownKind::FileTooLarge,
            format!("image too large: max {MAX_UPLOAD_BYTES} bytes"),
        )
        .with_param(
            "max",
            crate::commands::media_file::human_bytes(MAX_UPLOAD_BYTES as u64),
        ));
    }
    Ok(bytes)
}

/// Write why a cover command failed to the log before the error goes back
/// to the webview. The dialog says one generic sentence for most of them,
/// so without this line a failure leaves nothing to go on (#1173). The
/// error's text names the step and the OS's reason; of the paths, only a
/// failed write to the artwork cache names its folder (it may have been
/// moved), never the picked file's.
fn logged<T>(
    command: &'static str,
    playlist_id: Option<i64>,
    result: AppResult<T>,
) -> AppResult<T> {
    if let Err(err) = &result {
        tracing::warn!(command, playlist_id, error = %err, "playlist cover: command failed");
    }
    result
}

/// Return a small, validated in-memory preview for a file selected before
/// the playlist exists. The asset protocol cannot serve arbitrary paths
/// outside app data, so the UI must not pass a picker path to convertFileSrc.
#[tauri::command]
pub async fn preview_playlist_cover_from_file(file_path: String) -> AppResult<String> {
    logged("preview", None, preview_inner(&file_path))
}

fn preview_inner(file_path: &str) -> AppResult<String> {
    let bytes = read_cover_bytes(file_path)?;
    if detect_image_format(&bytes).is_none() {
        return Err(AppError::shown(
            crate::error::ShownKind::UnsupportedImage,
            "invalid playlist cover image",
        ));
    }
    let source = image::load_from_memory(&bytes).map_err(|e| {
        AppError::shown(
            crate::error::ShownKind::UnsupportedImage,
            format!("decode cover preview: {e}"),
        )
    })?;
    let preview = source.resize_to_fill(320, 320, image::imageops::FilterType::Triangle);
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 80)
        .encode_image(&preview)
        .map_err(|e| AppError::Other(format!("encode cover preview: {e}")))?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(encoded)
    ))
}

/// Number of tiles in the auto-cover grid. Mirrors the 2×2 layout in
/// [`cover::build_composite_cover`] — anything below 4 falls through to a
/// strips layout, which still looks fine but isn't the Spotify look.
const AUTO_COVER_TILE_COUNT: usize = 4;

#[derive(FromRow)]
struct PlaylistFlags {
    is_smart: i64,
    smart_rules: Option<String>,
    cover_is_auto: i64,
}

impl PlaylistFlags {
    /// Whose cover it is: the user's for a playlist they curate or a
    /// smart playlist of their own rules, the generator's for a generated
    /// family. A smart row whose rules do not read as custom ones counts
    /// as generated, so nothing here writes over a cover a generator
    /// owns.
    fn cover_is_users(&self) -> bool {
        self.is_smart == 0
            || crate::commands::smart_playlists::is_custom_payload(self.smart_rules.as_deref())
    }
}

async fn playlist_flags(pool: &SqlitePool, playlist_id: i64) -> AppResult<Option<PlaylistFlags>> {
    Ok(
        sqlx::query_as("SELECT is_smart, smart_rules, cover_is_auto FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_optional(pool)
            .await?,
    )
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
    logged(
        "set_from_file",
        Some(playlist_id),
        set_from_file_inner(&state, playlist_id, &file_path).await,
    )
}

async fn set_from_file_inner(state: &AppState, playlist_id: i64, file_path: &str) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    ensure_user_cover(&pool, playlist_id).await?;

    let bytes = read_cover_bytes(file_path)?;
    if detect_image_format(&bytes).is_none() {
        return Err(AppError::shown(
            crate::error::ShownKind::UnsupportedImage,
            "unsupported image format (expected jpg/png/webp)",
        ));
    }
    // Decoded in memory: the codec comes from the bytes, and nothing goes
    // through the system temp directory, which a broken `TMP` makes
    // unwritable (#1173). An image with the right magic bytes that still
    // does not decode (cut short, a variant the decoder lacks) is the
    // user's to change, so it is said as one.
    let image = image::load_from_memory(&bytes)
        .map_err(|e| {
            AppError::shown(
                crate::error::ShownKind::UnsupportedImage,
                format!("cover image decode: {e}"),
            )
        })?
        .to_rgb8();
    // We don't keep the original format in the metadata cache because
    // every smart-playlist cover is JPEG. Re-encode through the compositor
    // pipeline (single-tile fill) for two wins:
    //   1. Format normalisation — every `cover_hash` resolves to .jpg, no
    //      branching on `playlist_artwork_hash + extension`.
    //   2. Resize-to-canvas — uploads larger than 640×640 get downscaled
    //      to the on-disk standard, capping disk usage and keeping the
    //      sidebar / carousel render fast.
    let hash = cover::build_single_cover(&image, &state.paths.metadata_artwork_dir)?;

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
    let result = async {
        let (pool, profile_id) = state.require_profile_snapshot().await?;
        ensure_user_cover(&pool, playlist_id).await?;
        regenerate_inner(&pool, &state.paths, profile_id, playlist_id, true, false).await
    }
    .await;
    logged("regenerate", Some(playlist_id), result)
}

/// Drop the manual cover and switch back to auto. Immediately re-runs the
/// auto-cover so the user sees a fresh composite instead of falling back
/// to the gradient tile until the next mutation.
#[tauri::command]
pub async fn clear_playlist_cover(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<()> {
    let result = async {
        let (pool, profile_id) = state.require_profile_snapshot().await?;
        ensure_user_cover(&pool, playlist_id).await?;
        update_cover(&pool, playlist_id, None, 1).await?;
        // Best-effort regen — a playlist with no artwork to compose keeps
        // the gradient tile, and that is no reason to undo the clear. A
        // failure to compose is still written to the log.
        if let Err(err) =
            regenerate_inner(&pool, &state.paths, profile_id, playlist_id, false, false).await
        {
            tracing::warn!(playlist_id, error = %err, "auto-cover regen after clear failed (non-fatal)");
        }
        Ok(())
    }
    .await;
    logged("clear", Some(playlist_id), result)
}

/// Hook called by playlist-mutation commands (add/remove/reorder/source)
/// and after a custom smart playlist's rules wrote its tracks. No-op when
/// the playlist is a generated family, the user has uploaded their own
/// cover (`cover_is_auto = 0`), or none of its tracks has an artwork to
/// compose. Reads image files: never call it inside a write transaction.
pub async fn maybe_regen_auto_cover(
    pool: &SqlitePool,
    paths: &AppPaths,
    profile_id: i64,
    playlist_id: i64,
) {
    let flags = match playlist_flags(pool, playlist_id).await {
        Ok(flags) => flags,
        Err(err) => {
            tracing::warn!(playlist_id, error = %err, "auto-cover: playlist flags unreadable");
            return;
        }
    };
    let Some(flags) = flags else { return };
    if !flags.cover_is_users() || flags.cover_is_auto == 0 {
        return;
    }
    // A custom smart playlist's tracks are its rules' answer, which can
    // become none: its mosaic then goes rather than show tracks it no
    // longer holds. A playlist the user emptied keeps its cover.
    let clear_when_empty = flags.is_smart != 0;
    if let Err(err) = regenerate_inner(
        pool,
        paths,
        profile_id,
        playlist_id,
        false,
        clear_when_empty,
    )
    .await
    {
        // Auto-regen failures are never fatal to the mutation that
        // triggered them — the user still gets their add/remove/reorder
        // outcome, just without the cover refresh.
        tracing::warn!(playlist_id, error = %err, "auto-cover regen failed (non-fatal)");
    }
}

/// [`maybe_regen_auto_cover`] for each playlist a refresh of the custom
/// smart playlists wrote, in order. A playlist deleted since is skipped.
pub async fn regen_auto_covers(
    pool: &SqlitePool,
    paths: &AppPaths,
    profile_id: i64,
    playlist_ids: &[i64],
) {
    for &playlist_id in playlist_ids {
        maybe_regen_auto_cover(pool, paths, profile_id, playlist_id).await;
    }
}

/// Compose the auto-cover from the first tracks and store it. `over_manual`
/// is the explicit regenerate command's: it replaces whatever cover the
/// playlist has. Otherwise the cover is stored only while the playlist is
/// still on auto, so an upload that lands while the tracks are read and
/// the tiles composed — a refresh pass runs this in the background after
/// a scan — is not written over.
///
/// With nothing to compose, the cover stays, unless `clear_when_empty`
/// and the playlist holds no track at all, where an auto-cover is
/// cleared ([`clear_emptied_auto_cover`]). Tracks without artwork are
/// not an empty playlist, and a read that failed is an error, so neither
/// loses the cover.
async fn regenerate_inner(
    pool: &SqlitePool,
    paths: &AppPaths,
    profile_id: i64,
    playlist_id: i64,
    over_manual: bool,
    clear_when_empty: bool,
) -> AppResult<()> {
    let paths_for_compose =
        top_track_artwork_paths(pool, paths, profile_id, playlist_id, AUTO_COVER_TILE_COUNT)
            .await?;
    if paths_for_compose.is_empty() {
        if clear_when_empty && clear_emptied_auto_cover(pool, playlist_id).await? {
            tracing::info!(
                playlist_id,
                "auto-cover cleared: the playlist holds no track"
            );
            return Ok(());
        }
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
    if !write_auto_cover(pool, playlist_id, &hash, over_manual).await? {
        tracing::info!(
            playlist_id,
            "auto-cover not stored: the playlist got a manual cover meanwhile"
        );
        return Ok(());
    }
    tracing::info!(playlist_id, %hash, tile_count = paths_for_compose.len(), "auto-cover regenerated");
    Ok(())
}

/// Drop the playlist's auto-cover if it holds no track, in one statement
/// so a track written meanwhile keeps it. A manual cover is never
/// touched. Whether the row was written.
async fn clear_emptied_auto_cover(pool: &SqlitePool, playlist_id: i64) -> AppResult<bool> {
    let now = chrono::Utc::now().timestamp_millis();
    let written = sqlx::query(
        "UPDATE playlist
            SET cover_hash = NULL,
                updated_at = ?
          WHERE id = ?
            AND cover_is_auto = 1
            AND cover_hash IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM playlist_track WHERE playlist_id = playlist.id)",
    )
    .bind(now)
    .bind(playlist_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(written > 0)
}

/// Store `hash` as the playlist's auto-cover; with `over_manual` false,
/// only if it is still on auto. Whether the row was written.
async fn write_auto_cover(
    pool: &SqlitePool,
    playlist_id: i64,
    hash: &str,
    over_manual: bool,
) -> AppResult<bool> {
    let now = chrono::Utc::now().timestamp_millis();
    let written = sqlx::query(
        "UPDATE playlist
            SET cover_hash    = ?,
                cover_is_auto = 1,
                updated_at    = ?
          WHERE id = ?
            AND (? OR cover_is_auto = 1)",
    )
    .bind(hash)
    .bind(now)
    .bind(playlist_id)
    .bind(over_manual)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(written > 0)
}

/// Pull artwork file paths for the first `take` tracks of the playlist
/// (in `position` order) that have an album cover in the per-profile
/// cache. Tracks without artwork are silently skipped — we walk in order
/// until `take` is hit or the source is exhausted. A failed read is an
/// error, not an empty list, so its caller keeps the cover.
async fn top_track_artwork_paths(
    pool: &SqlitePool,
    paths: &AppPaths,
    profile_id: i64,
    playlist_id: i64,
    take: usize,
) -> AppResult<Vec<PathBuf>> {
    #[derive(FromRow)]
    struct Row {
        hash: String,
        format: String,
    }
    // No LIMIT — we may need to walk the whole playlist when the first
    // N tracks share a cover or have a missing artwork file. Playlists
    // are bounded by user patience (rarely > a few thousand entries),
    // so the round-trip stays cheap.
    let rows: Vec<Row> = sqlx::query_as(
        r#"
        SELECT aw.hash   AS hash,
               aw.format AS format
          FROM playlist_track pt
          JOIN track t       ON t.id  = pt.track_id
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = COALESCE(al.artwork_id, t.artwork_id)
         WHERE pt.playlist_id = ?
           AND t.is_available = 1
           AND aw.hash IS NOT NULL
         ORDER BY pt.position ASC
        "#,
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await?;

    // Dedupe by artwork hash so a playlist whose first 4 tracks share a
    // cover collapses to a single full-canvas image instead of a 2×2
    // grid of identical thumbnails — matches Spotify's behaviour on
    // mono-album playlists. Two/three unique covers still pick the
    // strip layout the compositor already exposes.
    let artwork_dir = paths.profile_artwork_dir(profile_id);
    let mut out = Vec::with_capacity(take);
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(take);
    let mut missing = 0usize;
    for row in rows {
        if out.len() >= take {
            break;
        }
        if !seen.insert(row.hash.clone()) {
            continue;
        }
        let p = artwork_dir.join(format!("{}.{}", row.hash, row.format));
        if p.exists() {
            out.push(p);
        } else {
            missing += 1;
        }
    }
    if missing > 0 {
        tracing::info!(
            playlist_id,
            missing,
            "auto-cover: skipped artworks whose file is not in the cache"
        );
    }
    Ok(out)
}

/// Reject generated smart playlists and missing ids with a precise error.
/// Keeps every public command above honest about what they accept: a
/// user playlist and a custom smart playlist pass, a generated family is
/// refused in the user's language (the edit modal does not offer it the
/// cover, so this is said only to a caller that went around it).
async fn ensure_user_cover(pool: &SqlitePool, playlist_id: i64) -> AppResult<()> {
    match playlist_flags(pool, playlist_id).await? {
        None => Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        ))),
        Some(flags) if !flags.cover_is_users() => Err(AppError::shown(
            crate::error::ShownKind::GeneratedPlaylistCover,
            "generated smart playlists keep their own cover",
        )),
        Some(_) => Ok(()),
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

#[cfg(test)]
mod tests;
