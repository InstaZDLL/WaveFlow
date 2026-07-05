//! Animated album artwork (Phase 3) — motion covers from metadata plugins.
//!
//! The `waveflow:metadata/v1` world's `album-info` gained optional
//! `motion-cover-url` / `motion-cover-tall-url` fields at WIT 1.1.0 (Apple
//! Music-style looping video covers). This command fans a lookup out to
//! every enabled metadata plugin and returns the first motion URL found,
//! for the now-playing / immersive views to render behind the cover.
//!
//! Honours [`offline::is_offline`] — a plugin's own HTTP is already gated
//! by the host, but short-circuiting here avoids instantiating the wasm at
//! all when the network is off.

use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::offline;
use crate::state::AppState;

/// Resolved motion artwork for an album — the looping video URL(s) plus
/// which plugin produced them (attribution / diagnostics).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionArtwork {
    /// Square animated cover — a directly-playable progressive `.mp4`.
    /// The host renders it in a native `<video>` with no HLS.js, so a
    /// plugin with an HLS source resolves it to an mp4 rendition before
    /// returning (a bare `.m3u8` won't play on WebView2).
    pub square_url: String,
    /// Taller lock-screen variant, when the plugin offers one.
    pub tall_url: Option<String>,
    pub plugin_id: String,
}

/// Ask every enabled `waveflow:metadata` plugin for `album-info(artist,
/// album)` and return the first that carries a `motion-cover-url`. Returns
/// `None` when offline, when no metadata plugin is installed, or when none
/// has motion artwork for the album. A plugin that errors is logged and
/// skipped — one bad plugin never blocks the others.
#[tauri::command]
pub async fn fetch_album_motion_artwork(
    state: State<'_, AppState>,
    artist: String,
    album: String,
) -> AppResult<Option<MotionArtwork>> {
    if offline::is_offline() {
        return Ok(None);
    }

    let plugin_ids =
        super::plugins::enabled_plugin_ids_for_world(&state, "waveflow:metadata").await?;

    for plugin_id in plugin_ids {
        // Serialise against enable/uninstall/install for this id.
        let _guard = super::plugins::lock_plugin(&state, &plugin_id).await;

        let runtime = state.plugins.clone();
        let paths = state.paths.plugin_paths();
        let id_owned = plugin_id.clone();
        let artist_owned = artist.clone();
        let album_owned = album.clone();

        let info = tokio::task::spawn_blocking(move || {
            waveflow_core::plugin::runtime::metadata_album_info(
                &runtime,
                &paths,
                &id_owned,
                &artist_owned,
                &album_owned,
            )
        })
        .await
        .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;

        match info {
            Ok(info) => {
                if let Some(square_url) = info.motion_cover_url {
                    return Ok(Some(MotionArtwork {
                        square_url,
                        tall_url: info.motion_cover_tall_url,
                        plugin_id,
                    }));
                }
                // No motion artwork from this plugin — try the next.
            }
            Err(e) => {
                tracing::warn!(plugin_id, %e, "metadata album-info failed; skipping");
            }
        }
    }

    Ok(None)
}
