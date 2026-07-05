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
use waveflow_core::artwork::motion_cache;

use crate::error::{AppError, AppResult};
use crate::offline;
use crate::state::AppState;

/// `app_setting` key for the opt-in local motion-artwork cache (default off).
const CACHE_ENABLED_KEY: &str = "motion_artwork.cache_enabled";

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

    // Read the opt-in local-cache flag once up front. When on, the resolved
    // remote mp4 is downloaded into the shared LRU cache and the overlay is
    // pointed at the local file (offline + no re-download on the next play).
    let cache_locally = motion_cache_enabled(&state).await;
    let cache_dir = state.paths.motion_cache_dir.clone();

    let plugin_ids =
        super::plugins::enabled_plugin_ids_for_world(&state, "waveflow:metadata").await?;

    tracing::debug!(
        %artist,
        %album,
        plugins = plugin_ids.len(),
        cache = cache_locally,
        "resolving motion artwork"
    );

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
                if let Some(remote_square) = info.motion_cover_url {
                    // SSRF guard: a plugin's own HTTP is allowlisted, but the
                    // cache download runs in-process AND the URL is handed to
                    // the webview <video>, so reject a non-https / internal /
                    // loopback target here before either touches it.
                    if !motion_cache::is_safe_motion_url(&remote_square) {
                        tracing::warn!(
                            plugin_id,
                            "plugin returned an unsafe motion url; skipping"
                        );
                        continue;
                    }
                    // When the local cache is on, download the mp4 and point the
                    // overlay at the on-disk copy; fall back to the remote URL if
                    // the download fails so the feature degrades gracefully.
                    let square_url = if cache_locally {
                        match motion_cache::cache_mp4(
                            &cache_dir,
                            &remote_square,
                            motion_cache::DEFAULT_MAX_CACHE_BYTES,
                        )
                        .await
                        {
                            Ok(path) => path.to_string_lossy().into_owned(),
                            Err(e) => {
                                tracing::warn!(%e, "motion cache download failed; serving remote url");
                                remote_square
                            }
                        }
                    } else {
                        remote_square
                    };
                    tracing::info!(
                        plugin_id,
                        %artist,
                        %album,
                        cached = cache_locally,
                        "motion artwork resolved"
                    );
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

    tracing::debug!(%artist, %album, "no motion artwork from any metadata plugin");
    Ok(None)
}

// ----- local motion cache --------------------------------------------------

/// Read the opt-in local-cache flag from the shared `app.db`. Defaults to
/// `false` (off) — same bool parse convention as every other `app_setting`.
async fn motion_cache_enabled(state: &AppState) -> bool {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(CACHE_ENABLED_KEY)
    .fetch_optional(&state.app_db)
    .await
    .ok()
    .flatten()
    .map(|v| v == "true" || v == "1")
    .unwrap_or(false)
}

/// The motion-cache toggle state + current on-disk footprint, for the
/// Settings → Plugins card.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionCacheInfo {
    pub enabled: bool,
    pub size_bytes: u64,
    pub file_count: u64,
}

/// Read the toggle + cache footprint for the settings UI.
#[tauri::command]
pub async fn get_motion_cache_info(state: State<'_, AppState>) -> AppResult<MotionCacheInfo> {
    let enabled = motion_cache_enabled(&state).await;
    let dir = state.paths.motion_cache_dir.clone();
    let (size_bytes, file_count) = tokio::task::spawn_blocking(move || motion_cache::stats(&dir))
        .await
        .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    Ok(MotionCacheInfo {
        enabled,
        size_bytes,
        file_count,
    })
}

/// Toggle the opt-in local motion cache. Turning it OFF does not purge the
/// existing files — that's the explicit "Clear cache" action below.
#[tauri::command]
pub async fn set_motion_cache_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(CACHE_ENABLED_KEY)
    .bind(if enabled { "true" } else { "false" })
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Delete every cached motion mp4 (and any leftover `.part` temporaries).
#[tauri::command]
pub async fn clear_motion_cache(state: State<'_, AppState>) -> AppResult<()> {
    let dir = state.paths.motion_cache_dir.clone();
    tokio::task::spawn_blocking(move || motion_cache::clear(&dir))
        .await
        .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    Ok(())
}
