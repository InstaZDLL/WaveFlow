//! Rescanning the artist pictures that lie next to the music.

use std::path::Path;

use serde::Serialize;

use waveflow_core::scanner::{
    extract_artist_image, link_local_artist_image, link_va_artist_image, VARIOUS_ARTISTS_LABEL,
};

use crate::{
    db::lock_waiters,
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

/// Summary returned by [`rescan_local_artist_images`].
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistImageScanSummary {
    /// Number of artists checked (those without an existing `artwork_id`).
    pub considered: i64,
    /// Number of artists that now have a local sidecar image linked.
    pub linked: i64,
}

/// Walk every `artist` row that has no `artwork_id` and try to resolve a
/// sidecar image from any of their tracks' folders. Cheap on re-runs
/// because already-linked rows are excluded by the SQL filter and we
/// stop at the first track that yields a match.
///
/// Lets users who scanned their library before this feature shipped pick
/// up `artist.jpg` files without re-importing every folder.
#[tauri::command]
pub async fn rescan_local_artist_images(
    state: tauri::State<'_, AppState>,
) -> AppResult<ArtistImageScanSummary> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&artwork_dir)?;

    let va_canon = canonical_name(VARIOUS_ARTISTS_LABEL);
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, name, canonical_name FROM artist
          WHERE artwork_id IS NULL
            AND canonical_name != ?",
    )
    .bind(&va_canon)
    .fetch_all(&*pool)
    .await?;

    let mut summary = ArtistImageScanSummary {
        considered: rows.len() as i64,
        linked: 0,
    };

    // No transaction is open while a folder is read. Those reads can be
    // slow (a network mount, a spun-down disk), and a transaction holding
    // the write lock across them kept every other write out until its busy
    // timeout. The reads run off the async runtime; each artist found is
    // linked in a short transaction of its own (the artwork row and the
    // artist's link together), which few artists ever need.
    for (artist_id, _name, canon) in rows {
        let tracks: Vec<(String,)> = sqlx::query_as(
            "SELECT t.file_path FROM track t
               JOIN track_artist ta ON ta.track_id = t.id
              WHERE ta.artist_id = ? AND t.is_available = 1
              LIMIT 16",
        )
        .bind(artist_id)
        .fetch_all(&*pool)
        .await?;

        let dir = artwork_dir.clone();
        let cover = tokio::task::spawn_blocking(move || {
            tracks
                .iter()
                .find_map(|(path,)| extract_artist_image(Path::new(path), &canon, &dir))
        })
        .await
        .map_err(|e| AppError::Other(format!("artist image search failed: {e}")))?;

        if let Some(cover) = cover {
            let mut tx = lock_waiters::begin_write(&pool).await?;
            link_local_artist_image(&mut tx, artist_id, &cover).await?;
            tx.commit().await?;
            summary.linked += 1;
        }
    }

    // The main loop joins `track_artist`, which the "Various Artists"
    // sentinel never appears in (it's an album artist) — resolve its
    // sidecar via the album relationship instead (issue #292). The
    // rows query above already excluded VA via `canonical_name != ?`.
    // On a plain connection, as the scan does: it reads folders too, and
    // its one write needs no transaction around those reads.
    let mut conn = pool.acquire().await?;
    if let Some(linked) = link_va_artist_image(&mut conn, &artwork_dir).await? {
        summary.considered += 1;
        if linked {
            summary.linked += 1;
        }
    }

    tracing::info!(
        considered = summary.considered,
        linked = summary.linked,
        "rescan_local_artist_images complete",
    );
    Ok(summary)
}
