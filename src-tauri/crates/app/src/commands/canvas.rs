//! Per-track looping Canvas (issue #442) — a short muted mp4 the user sets
//! on a single track, played behind the now-playing view Spotify-Canvas
//! style. Mirrors the manual motion-cover path (issue #408,
//! [`super::motion_artwork`]) but keyed per TRACK instead of per album, and
//! stored in its own [`AppPaths::profile_canvas_dir`] namespace.
//!
//! Core-only in spirit: nothing here touches a plugin or the network. The
//! file is picked by the user, validated by magic bytes, hash-addressed into
//! a never-evicted directory, and surfaced back to the webview as a local
//! path the `<video>` overlay converts through the asset protocol.

use serde::Serialize;

use tauri::State;

use crate::error::AppResult;
use crate::state::AppState;

/// Hard cap on a user-supplied Canvas clip, mirroring
/// [`super::motion_artwork`]'s manual-cover cap: a deliberately-chosen file
/// deserves a generous ceiling, but still needs *a* limit since this
/// directory is never evicted.
const MAX_CANVAS_MP4_BYTES: u64 = 64 * 1024 * 1024;

/// A track's Canvas clip, resolved to a local absolute path the webview
/// renders through `convertFileSrc`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackCanvas {
    /// Absolute path to the on-disk mp4 in the profile's `canvas/` dir.
    pub local_path: String,
}

/// Look up `track_id`'s Canvas clip, if one was set via
/// [`set_track_canvas_from_file`]. Returns `None` when the track has none —
/// callers render the static cover in that case.
#[tauri::command]
pub async fn get_track_canvas(
    state: State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<TrackCanvas>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT hash, format FROM track_canvas WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
    let Some((hash, format)) = row else {
        return Ok(None);
    };
    let path = state
        .paths
        .profile_canvas_dir(profile_id)
        .join(format!("{hash}.{format}"));
    Ok(Some(TrackCanvas {
        local_path: path.to_string_lossy().into_owned(),
    }))
}

/// Set `track_id`'s Canvas from a local mp4 file, replacing any previous
/// one. Validated by magic bytes (rejecting at pick time beats rendering a
/// black rectangle) and size-capped, then hash-addressed into the
/// never-evicted [`AppPaths::profile_canvas_dir`].
#[tauri::command]
pub async fn set_track_canvas_from_file(
    state: State<'_, AppState>,
    track_id: i64,
    file_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let canvas_dir = state.paths.profile_canvas_dir(profile_id);

    let hash =
        super::media_file::store_hash_addressed_mp4(&canvas_dir, &file_path, MAX_CANVAS_MP4_BYTES)
            .await?;

    sqlx::query(
        "INSERT INTO track_canvas (track_id, hash, format, created_at)
         VALUES (?, ?, 'mp4', ?)
         ON CONFLICT(track_id) DO UPDATE SET
            hash = excluded.hash, format = excluded.format, created_at = excluded.created_at",
    )
    .bind(track_id)
    .bind(&hash)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&*pool)
    .await?;

    Ok(())
}

/// Clear `track_id`'s Canvas, if any — the now-playing view falls back to
/// the static cover. The old file is left on disk (same "future GC pass"
/// tradeoff as the manual motion cover); a no-op when there was nothing to
/// clear.
#[tauri::command]
pub async fn clear_track_canvas(state: State<'_, AppState>, track_id: i64) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    sqlx::query("DELETE FROM track_canvas WHERE track_id = ?")
        .bind(track_id)
        .execute(&*pool)
        .await?;
    Ok(())
}
