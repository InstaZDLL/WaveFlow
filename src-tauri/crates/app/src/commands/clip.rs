//! A track's music video (discussion #1073): the video file named like the
//! track, beside it or in a `Clips` folder beside it, or the track's own
//! file when it is a video. Found on the disk at each asking, by
//! [`waveflow_core::scanner::clips::find_clip`]; nothing is stored.
//!
//! The clip plays in the webview with its own sound and its own length.
//! It is not the album version, so nothing ties it to the track's
//! timeline: the frontend pauses the track while it plays and resumes it
//! after. What this module adds besides the lookup is the flag that lets a
//! play / pause from outside the window (media keys, tray, taskbar,
//! global shortcut) reach the clip instead of starting the track under it
//! — see [`crate::player_actions::set_clip_open`].

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, State};
use waveflow_core::scanner::clips::{self, ClipOrigin};

use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackClip {
    /// Absolute path of the video, for `convertFileSrc` (or the loopback
    /// server on Linux).
    pub local_path: String,
    pub origin: ClipOrigin,
}

/// `track_id`'s clip, or `None`. A track on a network share or from the
/// server has none in this version: the webview reads neither a share
/// locator nor a server stream as a local file.
#[tauri::command]
pub async fn get_track_clip(
    app: AppHandle,
    state: State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<TrackClip>> {
    let pool = state.require_profile_pool().await?;
    let row: Option<(String,)> = sqlx::query_as("SELECT file_path FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&*pool)
        .await?;
    let Some((file_path,)) = row else {
        return Ok(None);
    };
    if waveflow_core::network_locator::is_locator(&file_path) {
        return Ok(None);
    }
    // A directory listing or two and a few header reads: off the runtime,
    // since the folder can be a spun-down drive.
    let found = tokio::task::spawn_blocking(move || clips::find_clip(&PathBuf::from(file_path)))
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(?err, "clip lookup task failed");
            None
        });
    let Some((path, origin)) = found else {
        return Ok(None);
    };
    // The file alone, not its folder: the static asset scope names the
    // app's own directories, and an album folder has no reason to become
    // readable by the webview as a whole.
    if let Err(err) = tauri::Manager::asset_protocol_scope(&app).allow_file(&path) {
        tracing::warn!(%err, "could not allow a clip in the asset scope");
    }
    Ok(Some(TrackClip {
        local_path: path.to_string_lossy().into_owned(),
        origin,
    }))
}

/// The frontend opened or closed a clip. While one is open, a play / pause
/// from outside the window drives the clip (a `clip:control` event)
/// instead of the track.
#[tauri::command]
pub fn player_set_clip_open(open: bool) {
    crate::player_actions::set_clip_open(open);
}
