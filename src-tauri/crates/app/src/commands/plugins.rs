//! Plugin SDK Tauri commands (Phase 3.1 — backend).
//!
//! Surfaces the install-dir contents + per-plugin enabled toggle +
//! uninstall path to the frontend. Install flow lives in a future
//! phase — for v1.5.0 the user sideloads a plugin directory by hand
//! (or it ships pre-installed with a future official channel) and
//! these commands handle everything else.
//!
//! Every command takes the runtime through `AppState::plugins`
//! rather than spinning a new `PluginRuntime` per call — the engine
//! is heavy (cranelift JIT) and reuse keeps the shared `Arc<Engine>`
//! warm across the session.

use std::fs;

use serde::Serialize;
use tauri::State;
use waveflow_core::plugin::manifest::Manifest;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Frontend-facing summary of one installed plugin. Mirrors the
/// manifest's public-ish fields + carries the per-plugin enabled
/// flag from `app_setting`. We don't echo `schema_version` to the
/// frontend — it's an internal protocol detail the UI has no use
/// for.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub world: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub permissions: PluginPermissionsInfo,
    pub assets: Vec<PluginAssetInfo>,
    /// `true` when the host should instantiate this plugin. Driven
    /// by `app_setting['plugin.<id>.enabled']`; missing key = on
    /// (an installed plugin is enabled by default).
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPermissionsInfo {
    pub http: Vec<String>,
    pub storage_read: bool,
    pub storage_state: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAssetInfo {
    pub filename: String,
    pub description: Option<String>,
}

fn manifest_to_info(manifest: Manifest, enabled: bool) -> PluginInfo {
    PluginInfo {
        id: manifest.plugin.id,
        name: manifest.plugin.name,
        version: manifest.plugin.version,
        author: manifest.plugin.author,
        world: manifest.plugin.world,
        description: manifest.plugin.description,
        homepage: manifest.plugin.homepage,
        license: manifest.plugin.license,
        permissions: PluginPermissionsInfo {
            http: manifest.permissions.http,
            storage_read: manifest.permissions.storage_read,
            storage_state: manifest.permissions.storage_state,
        },
        assets: manifest
            .assets
            .into_iter()
            .map(|a| PluginAssetInfo {
                filename: a.filename,
                description: a.description,
            })
            .collect(),
        enabled,
    }
}

/// Key shape for the per-plugin enabled toggle in `app_setting`.
/// One row per installed plugin — uninstall removes the row.
fn enabled_key(plugin_id: &str) -> String {
    format!("plugin.{plugin_id}.enabled")
}

async fn read_enabled(app_db: &sqlx::SqlitePool, plugin_id: &str) -> AppResult<bool> {
    // Missing row = enabled. We don't pre-populate on install so a
    // brand-new plugin always starts on; the user has to flip the
    // toggle to opt-out.
    let row: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(enabled_key(plugin_id))
    .fetch_optional(app_db)
    .await?;
    Ok(row.map(|v| v == "true" || v == "1").unwrap_or(true))
}

/// List every plugin installed under `<app-data>/waveflow/plugins/`.
///
/// Iterates the install root, parses each subdirectory's
/// `manifest.toml`, and returns a `PluginInfo` per valid plugin.
/// Subdirectories with a missing or malformed manifest are silently
/// skipped + logged at warn level — listing must never fail because
/// one entry is corrupt; the user should still be able to see (and
/// uninstall) their other plugins.
///
/// The FS walk + TOML parse run on a blocking thread (each manifest
/// is a `read_to_string` + `toml::from_str`, both sync); the
/// per-plugin `app_setting` lookup stays on the async side so the
/// SQLite pool's lock contention isn't pulled into the blocking pool.
#[tauri::command]
pub async fn list_installed_plugins(state: State<'_, AppState>) -> AppResult<Vec<PluginInfo>> {
    let paths = state.paths.plugin_paths();
    let manifests = tokio::task::spawn_blocking(move || -> AppResult<Vec<(String, Manifest)>> {
        let mut out = Vec::new();
        let entries = match fs::read_dir(&paths.plugins_root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(AppError::Io(e)),
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            let Some(plugin_id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string)
            else {
                continue;
            };
            let manifest_path = dir.join("manifest.toml");
            match Manifest::load_from_path(&manifest_path) {
                Ok(manifest) => {
                    // Pin: install dir name MUST match the manifest's
                    // declared id. The runtime refuses to load a
                    // mismatched plugin (Phase 2b's load-time guard),
                    // so skip it here too rather than surfacing a
                    // dangling row the user can't actually run.
                    if manifest.plugin.id != plugin_id {
                        tracing::warn!(
                            plugin_id,
                            manifest_id = %manifest.plugin.id,
                            "skipping plugin with id mismatch between dir and manifest"
                        );
                        continue;
                    }
                    out.push((plugin_id, manifest));
                }
                Err(err) => {
                    tracing::warn!(plugin_id, ?err, "skipping unreadable plugin manifest");
                }
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;

    let mut out = Vec::with_capacity(manifests.len());
    for (plugin_id, manifest) in manifests {
        let enabled = read_enabled(&state.app_db, &plugin_id).await?;
        out.push(manifest_to_info(manifest, enabled));
    }

    // Stable order so the frontend gets the same list across calls
    // without sorting itself.
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// Return a single plugin's manifest info. Useful for the Settings
/// detail page that opens when the user clicks a list row — avoids
/// re-fetching the whole list to surface one card.
#[tauri::command]
pub async fn get_plugin_info(
    state: State<'_, AppState>,
    plugin_id: String,
) -> AppResult<Option<PluginInfo>> {
    let paths = state.paths.plugin_paths();
    let manifest_path = match paths.manifest_path(&plugin_id) {
        Ok(p) => p,
        Err(_) => return Ok(None), // id failed sanitisation → no such plugin
    };
    let id_for_blocking = plugin_id.clone();
    let manifest_opt = tokio::task::spawn_blocking(move || -> Option<Manifest> {
        Manifest::load_from_path(&manifest_path)
            .ok()
            .filter(|m| m.plugin.id == id_for_blocking)
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    match manifest_opt {
        Some(manifest) => {
            let enabled = read_enabled(&state.app_db, &plugin_id).await?;
            Ok(Some(manifest_to_info(manifest, enabled)))
        }
        None => Ok(None),
    }
}

/// Flip the per-plugin enabled toggle. Doesn't unload a running
/// instance — Phase 4 will reload-on-flip when the player has
/// active plugin sources; for now the toggle is consulted by the
/// host before instantiation.
///
/// Refuses to write the toggle for a plugin that isn't actually
/// installed — a missing `manifest.toml` means the frontend asked
/// for an id that doesn't exist on disk, and silently creating the
/// row would leave an orphan in `app_setting` that survives
/// reinstall cycles. The id-shape validation is the second line of
/// defence; the existence check is the first.
#[tauri::command]
pub async fn set_plugin_enabled(
    state: State<'_, AppState>,
    plugin_id: String,
    enabled: bool,
) -> AppResult<()> {
    let paths = state.paths.plugin_paths();
    // Validate the id shape via PluginPaths. Refuses absolute paths,
    // `..` segments, embedded separators — same contract
    // `list_installed_plugins` uses.
    let plugin_dir = paths
        .plugin_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;

    // Existence gate runs on a blocking thread (`exists()` is a
    // syscall on every OS). If the install dir or manifest is
    // missing, we refuse the write so a stale frontend cache can't
    // sneak a setting in for a plugin the user already
    // uninstalled.
    let manifest_present = tokio::task::spawn_blocking(move || {
        plugin_dir.is_dir() && plugin_dir.join("manifest.toml").is_file()
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    if !manifest_present {
        return Err(AppError::Other(format!(
            "plugin not installed: {plugin_id}"
        )));
    }

    sqlx::query(
        "INSERT INTO app_setting (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(enabled_key(&plugin_id))
    .bind(if enabled { "true" } else { "false" })
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Remove a plugin entirely: install directory under
/// `<root>/plugins/<id>/` AND the scratch tree under
/// `<root>/plugin-data/<id>/`. The enabled flag row in `app_setting`
/// is also dropped so a future reinstall of the same id starts in
/// its default state (enabled).
///
/// Frontend must confirm with the user before invoking — the
/// command itself takes no "are you sure" parameter, on the same
/// principle as `delete_profile`.
#[tauri::command]
pub async fn uninstall_plugin(
    state: State<'_, AppState>,
    plugin_id: String,
) -> AppResult<()> {
    let paths = state.paths.plugin_paths();
    let install_dir = paths
        .plugin_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;
    let state_dir = paths
        .state_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;

    // Remove install + state on a blocking thread — `remove_dir_all`
    // on a multi-MB plugin tree (e.g. Web Radio embedding a ~10 MB
    // SQLite) can stretch into double-digit milliseconds and would
    // otherwise tie up a tokio worker. Missing dirs are not an
    // error — the user might be cleaning up a half-installed plugin
    // where one tree already went away.
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        if install_dir.exists() {
            fs::remove_dir_all(&install_dir)?;
        }
        if state_dir.exists() {
            fs::remove_dir_all(&state_dir)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;

    // Drop the enabled flag so a reinstall of the same id starts
    // fresh. ON CONFLICT not needed — DELETE on a missing row is
    // a no-op.
    sqlx::query("DELETE FROM app_setting WHERE key = ?")
        .bind(enabled_key(&plugin_id))
        .execute(&state.app_db)
        .await?;

    tracing::info!(plugin_id, "plugin uninstalled");
    Ok(())
}

