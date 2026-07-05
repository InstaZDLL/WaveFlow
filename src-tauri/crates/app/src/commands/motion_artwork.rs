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
use std::time::Duration;

use tauri::State;

use crate::error::AppResult;
use crate::offline;
use crate::state::AppState;

/// Per-plugin wall-clock bound on one `album-info` lookup. A cold Apple
/// resolve is a handful of sequential host HTTP GETs (each already capped
/// at 15 s by the host client); this caps the whole chain so one slow or
/// hung plugin can't stall the now-playing path. Generous because the
/// overlay is non-blocking UI (the static cover shows meanwhile) and a
/// resolved result is cached plugin-side after the first hit.
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(20);

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
/// album)` **concurrently** and return the first that carries a
/// `motion-cover-url`. Returns `None` when offline, when no metadata plugin
/// is installed, or when none has motion artwork for the album.
///
/// The lookups fan out (each on its own blocking task) and the first hit
/// wins; the rest are dropped. Every lookup is bounded by [`PLUGIN_TIMEOUT`],
/// so one slow or hung plugin can't stall the others or the now-playing
/// path. The pre-spawn loop only grabs each plugin's lock HANDLE (a fast
/// map op, never blocking on a contended plugin); the guard itself is taken
/// INSIDE the blocking task (`blocking_lock_owned`) so it spans the actual,
/// uncancellable work — the timeout unblocks the caller, but the guard is
/// only released when the work truly finishes, so an enable/uninstall can't
/// race an in-flight lookup. A plugin that errors, panics, or times out is
/// logged + skipped.
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

    let mut set = tokio::task::JoinSet::new();
    for plugin_id in plugin_ids {
        // Grab the per-plugin lock HANDLE only (fast map op) — the loop must
        // not block on a contended plugin. The guard is acquired inside the
        // blocking task below so it spans the real work.
        let lock_arc = super::plugins::plugin_lock_arc(&state, &plugin_id).await;
        let runtime = state.plugins.clone();
        let paths = state.paths.plugin_paths();
        let id_owned = plugin_id.clone();
        let artist_owned = artist.clone();
        let album_owned = album.clone();

        set.spawn(async move {
            let outcome = tokio::time::timeout(
                PLUGIN_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    // Acquire + hold the per-plugin lock for the ACTUAL
                    // duration of the (uncancellable) call. Because it lives
                    // inside the blocking closure, the guard is released only
                    // when `metadata_album_info` returns — even if the outer
                    // async timeout already fired — so an enable/uninstall
                    // can't race an in-flight lookup after an early drop.
                    let _guard = lock_arc.blocking_lock_owned();
                    waveflow_core::plugin::runtime::metadata_album_info(
                        &runtime,
                        &paths,
                        &id_owned,
                        &artist_owned,
                        &album_owned,
                    )
                }),
            )
            .await;
            (plugin_id, outcome)
        });
    }

    // Consume results as they complete; first motion cover wins (remaining
    // tasks are aborted when `set` drops on the early return).
    while let Some(joined) = set.join_next().await {
        let (plugin_id, outcome) = match joined {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%e, "motion artwork task join failed; skipping");
                continue;
            }
        };
        match outcome {
            Ok(Ok(Ok(info))) => {
                if let Some(square_url) = info.motion_cover_url {
                    return Ok(Some(MotionArtwork {
                        square_url,
                        tall_url: info.motion_cover_tall_url,
                        plugin_id,
                    }));
                }
                // Reached the plugin, no motion for this album — keep going.
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(plugin_id, %e, "metadata album-info failed; skipping");
            }
            Ok(Err(e)) => {
                tracing::warn!(plugin_id, %e, "metadata lookup task panicked; skipping");
            }
            Err(_elapsed) => {
                tracing::warn!(plugin_id, "metadata lookup timed out; skipping");
            }
        }
    }

    Ok(None)
}
