//! The `source` world: browsing a plugin's entries and resolving a track
//! to a stream URL.

use serde::Serialize;
use tauri::State;
use tokio::sync::OwnedMutexGuard;
use waveflow_core::plugin::runtime::{source_list_entries, source_resolve, source_stream_url};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::*;

// ----- plugin invocation surface -----------------------------------------
//
// Three commands wrap the `waveflow:source/provider` exports. Each
// reloads the component from disk + builds a fresh Linker + Store
// per call — the wasmtime Engine itself is cached on `AppState`,
// so the heavy backend setup is paid once at app boot and every
// invocation only pays the per-instantiation cost (~10 ms for our
// 139 KB Web Radio component). Phase 5 will cache the LoadedPlugin
// + Linker per plugin id when a real perf complaint surfaces.

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEntry {
    pub label: String,
    pub query: String,
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// 0 for live streams (radio); the UI hides the seek bar when
    /// the value is 0 and shows "LIVE" instead.
    pub duration_ms: u32,
    pub artwork_url: Option<String>,
    pub icy_url: Option<String>,
}

/// Pre-flight checks every source-invocation command shares:
/// char-class gate on the id, acquire the per-plugin lock (so
/// `set_plugin_enabled` / `uninstall_plugin` can't race us mid-
/// invocation), refuse if the user disabled the plugin in
/// Settings.
pub(super) async fn source_preamble(
    state: &AppState,
    plugin_id: &str,
) -> AppResult<OwnedMutexGuard<()>> {
    validate_plugin_id_chars(plugin_id)?;
    let guard = lock_plugin(state, plugin_id).await;
    if !read_enabled(&state.app_db, plugin_id).await? {
        return Err(AppError::Other(format!("plugin disabled: {plugin_id}")));
    }
    Ok(guard)
}

/// List the top-level categories the plugin exposes. Backs the
/// Web Radio sidebar entry — the host renders one row per entry,
/// clicks call `plugin_resolve` with the entry's `query`.
#[tauri::command]
pub async fn plugin_list_entries(
    state: State<'_, AppState>,
    plugin_id: String,
) -> AppResult<Vec<PluginEntry>> {
    let _guard = source_preamble(&state, &plugin_id).await?;
    let runtime = state.plugins.clone();
    let paths = state.paths.plugin_paths();
    let id_owned = plugin_id.clone();
    let entries = tokio::task::spawn_blocking(move || {
        source_list_entries(&runtime, &paths, &id_owned)
            .map_err(|e| AppError::Other(format!("plugin {plugin_id}: {}", e.detail())))
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;
    Ok(entries
        .into_iter()
        .map(|e| PluginEntry {
            label: e.label,
            query: e.query,
            icon_url: e.icon_url,
        })
        .collect())
}

/// Resolve a category / search query to tracks. The plugin defines
/// the wire format of `query`; the host treats it as opaque.
#[tauri::command]
pub async fn plugin_resolve(
    state: State<'_, AppState>,
    plugin_id: String,
    query: String,
) -> AppResult<Vec<PluginTrack>> {
    let _guard = source_preamble(&state, &plugin_id).await?;
    let runtime = state.plugins.clone();
    let paths = state.paths.plugin_paths();
    let id_owned = plugin_id.clone();
    let tracks = tokio::task::spawn_blocking(move || {
        source_resolve(&runtime, &paths, &id_owned, &query)
            .map_err(|e| AppError::Other(format!("plugin {plugin_id}: {}", e.detail())))
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;
    Ok(tracks
        .into_iter()
        .map(|t| PluginTrack {
            id: t.id,
            title: t.title,
            artist: t.artist,
            album: t.album,
            duration_ms: t.duration_ms,
            artwork_url: t.artwork_url,
            icy_url: t.icy_url,
        })
        .collect())
}

/// Mint the playable stream URL for one track. Called at play
/// time so plugins that issue short-lived tokens (auth-gated
/// streams) get a fresh URL on every play.
#[tauri::command]
pub async fn plugin_stream_url(
    state: State<'_, AppState>,
    plugin_id: String,
    track_id: String,
) -> AppResult<String> {
    let _guard = source_preamble(&state, &plugin_id).await?;
    let runtime = state.plugins.clone();
    let paths = state.paths.plugin_paths();
    let id_owned = plugin_id.clone();
    let url = tokio::task::spawn_blocking(move || {
        source_stream_url(&runtime, &paths, &id_owned, &track_id)
            .map_err(|e| AppError::Other(format!("plugin {plugin_id}: {}", e.detail())))
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;
    Ok(url)
}
