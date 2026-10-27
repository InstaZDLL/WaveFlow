//! The genre list and the artwork a genre can be given.

use std::io::Read;
use std::path::PathBuf;

use serde::Serialize;
use sqlx::FromRow;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Slim genre row shipped by `list_genres` — same `artwork_base` economy
/// as `AlbumRow`: hash/format ride on the row, the per-profile prefix
/// rides once on the response.
#[derive(Debug, Clone, Serialize)]
pub struct GenreRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListGenresResponse {
    pub artwork_base: String,
    pub items: Vec<GenreRow>,
}

#[derive(FromRow)]
struct GenreRawRow {
    id: i64,
    name: String,
    track_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

/// List every genre that tags at least one available track in the given
/// library, with a track count.
#[tauri::command]
pub async fn list_genres(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
) -> AppResult<ListGenresResponse> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let raw = sqlx::query_as::<_, GenreRawRow>(
        r#"
        SELECT g.id,
               g.name,
               COUNT(DISTINCT t.id) AS track_count,
               aw.hash              AS artwork_hash,
               aw.format            AS artwork_format
          FROM genre g
          JOIN track_genre tg ON tg.genre_id = g.id
          JOIN track t         ON t.id = tg.track_id
          LEFT JOIN artwork aw ON aw.id = g.artwork_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY g.id
         ORDER BY g.canonical_name COLLATE FOLD
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .fetch_all(&*pool)
    .await?;

    let items = expand_genre_rows(raw, artwork_dir.clone()).await?;

    Ok(ListGenresResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch thumbnail-existence flags onto a batch of raw genre rows —
/// same off-thread batching rationale as `expand_album_rows`.
async fn expand_genre_rows(
    raw: Vec<GenreRawRow>,
    artwork_dir: PathBuf,
) -> AppResult<Vec<GenreRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|row| {
                let (artwork_has_1x, artwork_has_2x) = match row.artwork_hash.as_deref() {
                    Some(hash) => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                GenreRow {
                    id: row.id,
                    name: row.name,
                    track_count: row.track_count,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("genre row expand join: {e}")))
}

/// Manually upload an image file as a genre's picture (issue #424) — a
/// local jpg/png/webp the user picks themselves; genres have no
/// automatic/embedded artwork source of their own, unlike album/artist.
/// Same magic-byte validation as `set_artist_artwork_from_file`.
#[tauri::command]
pub async fn set_genre_artwork_from_file(
    state: tauri::State<'_, AppState>,
    genre_id: i64,
    file_path: String,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // Off the async runtime: create_dir_all/read/write are all blocking
    // syscalls, and the read is bounded to MAX_IMAGE_BYTES + 1 so an
    // oversized file is rejected without first loading the whole thing
    // into memory.
    let dir_for_blocking = profile_artwork_dir.clone();
    let (hash, format): (String, &'static str) = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir_for_blocking)?;

        let mut file = std::fs::File::open(&file_path)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(crate::commands::deezer::MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > crate::commands::deezer::MAX_IMAGE_BYTES {
            // Same reasoning as `media_file::store_hash_addressed_mp4`:
            // the capped read knows only "over", so stat for the number
            // worth printing.
            let actual = std::fs::metadata(&file_path)
                .map(|m| format!("{} ", crate::commands::media_file::human_bytes(m.len())))
                .unwrap_or_default();
            return Err(AppError::Other(format!(
                "file too large: {actual}(max {})",
                crate::commands::media_file::human_bytes(
                    crate::commands::deezer::MAX_IMAGE_BYTES as u64
                )
            )));
        }
        let format =
            waveflow_core::artwork::image_format::detect_image_format(&bytes).ok_or_else(|| {
                AppError::Other("unsupported image format (expected jpg/png/webp)".into())
            })?;

        let hash = blake3::hash(&bytes).to_hex().to_string();
        let target = dir_for_blocking.join(format!("{hash}.{format}"));
        waveflow_core::atomic_write::store_content_addressed(&target, &bytes)?;
        Ok::<_, AppError>((hash, format))
    })
    .await
    .map_err(|e| AppError::Other(format!("genre artwork blocking task join: {e}")))??;

    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    let mut tx = pool.begin().await?;
    let artwork_id = waveflow_core::scanner::upsert_artwork(
        &mut tx,
        &hash,
        format,
        waveflow_core::scanner::ArtworkSource::Manual,
    )
    .await?;
    let res = sqlx::query("UPDATE genre SET artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(genre_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("genre {genre_id} not found")));
    }
    tx.commit().await?;

    Ok(())
}

/// Detach a genre's manual picture. The orphaned `artwork` row (if no
/// longer referenced) is left in place — same future-GC-pass note as
/// `clear_artist_artwork`.
#[tauri::command]
pub async fn clear_genre_artwork(
    state: tauri::State<'_, AppState>,
    genre_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let res = sqlx::query("UPDATE genre SET artwork_id = NULL WHERE id = ?")
        .bind(genre_id)
        .execute(&*pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("genre {genre_id} not found")));
    }
    Ok(())
}
