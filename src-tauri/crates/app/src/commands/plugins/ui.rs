//! The `ui` world: the registered mount points, the redacted library
//! snapshot a plugin may read, and its render and event calls.

use serde::Serialize;
use tauri::State;
use tokio::sync::OwnedMutexGuard;
use waveflow_core::plugin::manifest::Manifest;
use waveflow_core::plugin::runtime::{ui_event, ui_manifest, ui_render, LibraryArtist};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::*;

// ----- ui world invocation surface (Phase 5 — waveflow:ui/v1) --------------
//
// A UI plugin describes its views as JSON descriptors the frontend
// renders with native React components (no code injection). Three
// commands back the loop: `list_ui_plugins` (enumerate + place the
// sidebar entries), `plugin_ui_render` (the current view), and
// `plugin_ui_event` (round-trip a user action → next view). Render /
// event return the RAW descriptor string — parsing + validation live
// frontend-side. Each reloads the component per call, same as the
// source surface (a click is human-paced, not a hot loop).

/// Owned mirror of `waveflow:ui/extension/mount-point` for the
/// frontend — where a UI plugin plants its navigable sidebar entry.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUiMountPoint {
    pub sidebar_label: String,
    pub sidebar_icon: Option<String>,
    pub initial_path: String,
}

/// One enabled UI plugin + its sidebar registration. The frontend
/// builds one navigation entry per element, driving routing off the
/// plugin id rather than any hardcoded name.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUiRegistration {
    pub plugin_id: String,
    pub mount_point: PluginUiMountPoint,
}

/// SQLite projection for the redacted artist snapshot. `id` +
/// `track_count` come back as `i64` (SQLite's only integer width);
/// the map to `LibraryArtist` narrows them.
#[derive(sqlx::FromRow)]
struct ArtistSnapshotRow {
    id: i64,
    name: String,
    track_count: i64,
}

/// `true` iff the plugin's manifest declares the `library.read_artists`
/// permission. Read on the blocking pool (a small TOML parse), id-pinned
/// like the other manifest reads. A missing / unparsable / mismatched
/// manifest yields `false` — the host gate would deny the read anyway,
/// so we default closed.
async fn plugin_grants_library_read(state: &AppState, plugin_id: &str) -> AppResult<bool> {
    let paths = state.paths.plugin_paths();
    let manifest_path = match paths.manifest_path(plugin_id) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    let id = plugin_id.to_string();
    tokio::task::spawn_blocking(move || {
        Manifest::load_from_path(&manifest_path)
            .ok()
            .filter(|m| m.plugin.id == id)
            .map(|m| m.permissions.library_read_artists)
            .unwrap_or(false)
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))
}

/// Load the active profile's REDACTED artist snapshot for a `ui`
/// plugin: names + opaque ids + aggregate track counts, most-present
/// first, clamped to [`MAX_LIBRARY_ARTISTS`]. Loaded on the async side
/// BEFORE the blocking runtime call so the guest is handed a ready
/// snapshot and never touches the DB. Local read only — no network,
/// so it works offline.
///
/// Short-circuits to an empty list (skipping the profile pool + query
/// entirely) when the plugin never requested `library.read_artists` —
/// the host gate would deny the read anyway, so there's no reason to
/// materialize the redacted data for it. Authorized plugins keep the
/// capped query, so both callers inherit the optimization.
async fn load_library_artist_snapshot(
    state: &AppState,
    plugin_id: &str,
    limit: usize,
) -> AppResult<Vec<LibraryArtist>> {
    if !plugin_grants_library_read(state, plugin_id).await? {
        return Ok(Vec::new());
    }
    let pool = state.require_profile_pool().await?;
    let capped = limit.min(waveflow_core::plugin::host_impl::MAX_LIBRARY_ARTISTS) as i64;
    let rows = sqlx::query_as::<_, ArtistSnapshotRow>(
        "SELECT ar.id AS id,
                ar.name AS name,
                COUNT(DISTINCT t.id) AS track_count
           FROM artist ar
           JOIN track_artist ta ON ta.artist_id = ar.id
           JOIN track t ON t.id = ta.track_id
          WHERE t.is_available = 1
          GROUP BY ar.id, ar.name
          ORDER BY track_count DESC, ar.canonical_name COLLATE NOCASE ASC
          LIMIT ?",
    )
    .bind(capped)
    .fetch_all(&*pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| LibraryArtist {
            id: r.id.max(0) as u64,
            name: r.name,
            // Clamp to the u32 range before narrowing: a bare `as u32`
            // would wrap a value above u32::MAX. Unreachable in practice
            // (it's a track COUNT) but the cast should be lossless, not
            // silently truncating. `.max(0)` still floors negatives at 0.
            track_count: r.track_count.clamp(0, u32::MAX as i64) as u32,
        })
        .collect())
}

/// Pre-flight for a `ui`-world invocation — identical to
/// [`source_preamble`] (char-class gate + per-plugin lock + enabled
/// check), aliased for call-site clarity.
async fn ui_preamble(state: &AppState, plugin_id: &str) -> AppResult<OwnedMutexGuard<()>> {
    source_preamble(state, plugin_id).await
}

/// Confirm a plugin's `render` / `on-event` output is well-formed JSON
/// before it reaches the UI. A buggy guest that returns garbage
/// surfaces here as a clean, plugin-tagged error instead of a raw
/// `JSON.parse` throw frontend-side. Cheap (a single parse of a human-
/// paced view) and the frontend still validates `schemaVersion` itself.
fn validate_ui_descriptor(plugin_id: &str, descriptor: &str) -> AppResult<()> {
    serde_json::from_str::<serde_json::Value>(descriptor).map_err(|e| {
        AppError::Other(format!("plugin {plugin_id}: invalid view descriptor: {e}"))
    })?;
    Ok(())
}

/// Enumerate every enabled `ui`-world plugin and its sidebar mount
/// point. The frontend calls this to build its dynamic navigation —
/// one entry per registration, keyed on `plugin_id`. A plugin whose
/// `manifest()` traps is skipped + logged rather than failing the
/// whole sidebar build, so one broken plugin can't blank the nav.
#[tauri::command]
pub async fn list_ui_plugins(state: State<'_, AppState>) -> AppResult<Vec<PluginUiRegistration>> {
    let ids = enabled_plugin_ids_for_world(&state, waveflow_core::plugin::worlds::UI_V1).await?;
    let mut out = Vec::with_capacity(ids.len());
    for plugin_id in ids {
        let runtime = state.plugins.clone();
        let paths = state.paths.plugin_paths();
        let id_owned = plugin_id.clone();
        let mp = tokio::task::spawn_blocking(move || ui_manifest(&runtime, &paths, &id_owned))
            .await
            .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
        match mp {
            Ok(mp) => out.push(PluginUiRegistration {
                plugin_id,
                mount_point: PluginUiMountPoint {
                    sidebar_label: mp.sidebar_label,
                    sidebar_icon: mp.sidebar_icon,
                    initial_path: mp.initial_path,
                },
            }),
            Err(err) => {
                tracing::warn!(
                    plugin_id,
                    err = %err.detail(),
                    "ui plugin manifest() failed; skipping sidebar entry"
                );
            }
        }
    }
    Ok(out)
}

/// Render a UI plugin's view for an internal `path`. Returns the raw
/// JSON view descriptor string; the frontend parses + draws it.
#[tauri::command]
pub async fn plugin_ui_render(
    state: State<'_, AppState>,
    plugin_id: String,
    path: String,
) -> AppResult<String> {
    let _guard = ui_preamble(&state, &plugin_id).await?;
    let snapshot = load_library_artist_snapshot(
        &state,
        &plugin_id,
        waveflow_core::plugin::host_impl::MAX_LIBRARY_ARTISTS,
    )
    .await?;
    let runtime = state.plugins.clone();
    let paths = state.paths.plugin_paths();
    let id_owned = plugin_id.clone();
    let err_id = plugin_id.clone();
    let descriptor = tokio::task::spawn_blocking(move || {
        ui_render(&runtime, &paths, &id_owned, &path, snapshot)
            .map_err(|e| AppError::Other(format!("plugin {err_id}: {}", e.detail())))
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;
    validate_ui_descriptor(&plugin_id, &descriptor)?;
    Ok(descriptor)
}

/// Round-trip a user action (`event` + opaque `payload`) through a UI
/// plugin's `on-event`. Returns the NEXT raw JSON descriptor that
/// replaces the current view.
#[tauri::command]
pub async fn plugin_ui_event(
    state: State<'_, AppState>,
    plugin_id: String,
    event: String,
    payload: String,
) -> AppResult<String> {
    let _guard = ui_preamble(&state, &plugin_id).await?;
    let snapshot = load_library_artist_snapshot(
        &state,
        &plugin_id,
        waveflow_core::plugin::host_impl::MAX_LIBRARY_ARTISTS,
    )
    .await?;
    let runtime = state.plugins.clone();
    let paths = state.paths.plugin_paths();
    let id_owned = plugin_id.clone();
    let err_id = plugin_id.clone();
    let descriptor = tokio::task::spawn_blocking(move || {
        ui_event(&runtime, &paths, &id_owned, &event, &payload, snapshot)
            .map_err(|e| AppError::Other(format!("plugin {err_id}: {}", e.detail())))
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;
    validate_ui_descriptor(&plugin_id, &descriptor)?;
    Ok(descriptor)
}
