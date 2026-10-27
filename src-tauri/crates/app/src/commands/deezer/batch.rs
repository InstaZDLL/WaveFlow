//! Fetching the missing artist pictures and album covers in one pass.

use tauri::{AppHandle, Emitter};

use crate::{error::AppResult, state::AppState};

use super::*;

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
    // One snapshot, held for the whole loop: the pool and the artwork
    // dir have to describe the same profile from the first artist to
    // the last.
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
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
    .fetch_all(&*pool)
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
        // Same pool the artist list was read from — see
        // `enrich_artist_deezer_with_pool`.
        match enrich_artist_deezer_with_pool(state.clone(), &pool, &artwork_dir, artist_id).await {
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
            .fetch_all(&*pool)
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
