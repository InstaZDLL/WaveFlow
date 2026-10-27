//! Rescanning the artist pictures that lie next to the music.

use std::path::Path;

use serde::Serialize;

use waveflow_core::scanner::{
    extract_artist_image, link_local_artist_image, link_va_artist_image, VARIOUS_ARTISTS_LABEL,
};

use crate::{error::AppResult, state::AppState};

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

    // Batch writes through a single transaction (committed every
    // TX_BATCH writes) so SQLite WAL fsyncs once per batch instead of
    // once per artist — same pattern as scan_folder_inner.
    const TX_BATCH: usize = 200;
    let mut tx = pool.begin().await?;
    let mut tx_count: usize = 0;

    for (artist_id, _name, canon) in rows {
        // Track lookup is a read — run it on the pool so it doesn't
        // serialise behind the open write transaction.
        let tracks: Vec<(String,)> = sqlx::query_as(
            "SELECT t.file_path FROM track t
               JOIN track_artist ta ON ta.track_id = t.id
              WHERE ta.artist_id = ? AND t.is_available = 1
              LIMIT 16",
        )
        .bind(artist_id)
        .fetch_all(&*pool)
        .await?;

        let mut linked = false;
        for (path,) in tracks {
            if let Some(cover) = extract_artist_image(Path::new(&path), &canon, &artwork_dir) {
                link_local_artist_image(&mut tx, artist_id, &cover).await?;
                linked = true;
                break;
            }
        }
        if linked {
            summary.linked += 1;
            tx_count += 1;
            if tx_count >= TX_BATCH {
                tx.commit().await?;
                tx = pool.begin().await?;
                tx_count = 0;
            }
        }
    }

    // The main loop joins `track_artist`, which the "Various Artists"
    // sentinel never appears in (it's an album artist) — resolve its
    // sidecar via the album relationship instead (issue #292). The
    // rows query above already excluded VA via `canonical_name != ?`.
    if let Some(linked) = link_va_artist_image(&mut tx, &artwork_dir).await? {
        summary.considered += 1;
        if linked {
            summary.linked += 1;
        }
    }

    tx.commit().await?;

    tracing::info!(
        considered = summary.considered,
        linked = summary.linked,
        "rescan_local_artist_images complete",
    );
    Ok(summary)
}
