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
use std::sync::Arc;
use std::sync::atomic::Ordering as AtomicOrdering;

use serde::Serialize;
use tauri::State;
use tokio::sync::{Mutex, OwnedMutexGuard};
use waveflow_core::plugin::manifest::{Manifest, ManifestError};
use waveflow_core::plugin::runtime::{source_list_entries, source_resolve, source_stream_url};

use crate::audio::{AudioCmd, AudioEngine};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use waveflow_core::plugin::is_bundled_plugin;

/// Acquire the per-plugin write lock. Inserts a fresh `Mutex<()>`
/// into the runtime's map the first time we see this id; returns
/// an owned guard the caller holds for the duration of the
/// command. Two commands targeting the same `plugin_id` serialise;
/// commands on different ids run in parallel.
///
/// Hold this BEFORE the manifest-existence check in
/// `set_plugin_enabled` and BEFORE `remove_dir_all` in
/// `uninstall_plugin`. Releasing only at function end (guard
/// drop) keeps the manifest probe + the SQL UPSERT atomic against
/// a racing uninstall.
async fn lock_plugin(state: &AppState, plugin_id: &str) -> OwnedMutexGuard<()> {
    let arc_mutex: Arc<Mutex<()>> = {
        let mut map = state.plugin_locks.lock().await;
        map.entry(plugin_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    arc_mutex.lock_owned().await
}

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
    /// `true` when this plugin is shipped inside the installer and
    /// re-seeded at every boot. The UI replaces "Uninstall" with a
    /// disabled hint on bundled rows, and the backend refuses
    /// [`uninstall_plugin`] for the same id so the FS-remove + boot
    /// reseed cycle can't masquerade as an uninstall that "comes
    /// back" on next launch.
    pub bundled: bool,
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
    let bundled = is_bundled_plugin(&manifest.plugin.id);
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
        bundled,
    }
}

/// Key shape for the per-plugin enabled toggle in `app_setting`.
/// One row per installed plugin — uninstall removes the row.
fn enabled_key(plugin_id: &str) -> String {
    format!("plugin.{plugin_id}.enabled")
}

/// Enforce the same character class the manifest validator pins on
/// `plugin.id`: `[a-z0-9-]+`. Called at the top of every mutating
/// command BEFORE the lock map is touched so a case-variant call
/// (`"Foo"` on Windows / macOS where the FS is case-insensitive)
/// doesn't pollute `plugin_locks` with entries that can never lead
/// to a successful write — the per-id manifest byte-match later
/// in the pipeline would reject them anyway. Surfacing the
/// mismatch as a clear "illegal character" error here also gives
/// the frontend a single, unambiguous failure mode instead of the
/// downstream "manifest id mismatch" which reads like a sandbox
/// breach.
fn validate_plugin_id_chars(plugin_id: &str) -> AppResult<()> {
    if plugin_id.is_empty() {
        return Err(AppError::Other("plugin id is empty".into()));
    }
    for ch in plugin_id.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-';
        if !ok {
            return Err(AppError::Other(format!(
                "plugin id contains illegal character {ch:?} (allowed: [a-z0-9-])"
            )));
        }
    }
    Ok(())
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

/// Walk one install root and collect (id, manifest) pairs for every
/// subdirectory whose `manifest.toml` parses cleanly and whose
/// declared id matches the directory name. Missing dirs return an
/// empty vec (a fresh install has no sideload tree). Other IO errors
/// propagate.
///
/// Pure helper so [`list_installed_plugins`] can call it twice (once
/// for `bundled_root`, once for `plugins_root`) and merge.
fn walk_install_root(root: &std::path::Path) -> AppResult<Vec<(String, Manifest)>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(AppError::Io(e)),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(plugin_id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
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
}

/// List every plugin the runtime can load. Walks two roots:
///
/// 1. `<resource_dir>/plugins/` — installer-bundled tree (read-only).
///    Source of truth for first-party ids (currently `web-radio`).
/// 2. `<app-data>/waveflow/plugins/` — sideloaded tree (writable).
///    Holds anything the user installs themselves.
///
/// Bundled wins on collision: if a sideloaded dir somehow declares a
/// bundled id (shouldn't happen post-cleanup, but defence-in-depth),
/// the bundled copy is the one the runtime would actually load via
/// [`PluginPaths::plugin_dir`], so we surface that one and drop the
/// duplicate from the list. Sideloaded subdirectories with a missing
/// or malformed manifest are silently skipped + logged at warn level
/// — listing must never fail because one entry is corrupt.
///
/// The FS walk + TOML parse run on a blocking thread (each manifest
/// is a `read_to_string` + `toml::from_str`, both sync); the
/// per-plugin `app_setting` lookup stays on the async side so the
/// SQLite pool's lock contention isn't pulled into the blocking pool.
#[tauri::command]
pub async fn list_installed_plugins(state: State<'_, AppState>) -> AppResult<Vec<PluginInfo>> {
    let paths = state.paths.plugin_paths();
    let manifests = tokio::task::spawn_blocking(move || -> AppResult<Vec<(String, Manifest)>> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out = Vec::new();

        // Bundled tree FIRST so its ids own the slot on any collision
        // with a stray sideloaded entry. `bundled_root` is optional
        // because dev builds without a bundle still need to list
        // sideloaded plugins.
        if let Some(bundled_root) = paths.bundled_root.as_deref() {
            for (id, manifest) in walk_install_root(bundled_root)? {
                seen.insert(id.clone());
                out.push((id, manifest));
            }
        }

        // Sideloaded tree second; skip any id the bundled walk
        // already claimed.
        for (id, manifest) in walk_install_root(&paths.plugins_root)? {
            if seen.contains(&id) {
                tracing::warn!(
                    plugin_id = %id,
                    "sideloaded plugin shadows a bundled id; skipping the sideloaded copy"
                );
                continue;
            }
            out.push((id, manifest));
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
    out.sort_by_key(|a| a.name.to_lowercase());
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
    // Character-class gate FIRST: rejects case-variant input
    // before any lock entry is created. See doc on
    // `validate_plugin_id_chars`.
    validate_plugin_id_chars(&plugin_id)?;

    let paths = state.paths.plugin_paths();
    // Validate the id shape via PluginPaths. Refuses absolute paths,
    // `..` segments, embedded separators — same contract
    // `list_installed_plugins` uses.
    let plugin_dir = paths
        .plugin_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;

    // Serialise against `uninstall_plugin` for the same id. Without
    // this lock, a concurrent uninstall could remove the install
    // dir between our existence check and the UPSERT below, leaving
    // an orphan row exactly like the previous code path did.
    let _guard = lock_plugin(&state, &plugin_id).await;

    // Strong existence gate: PARSE the manifest and confirm its
    // declared id matches the param byte-for-byte. The manifest
    // validator restricts ids to `[a-z0-9-]+`, so this rejects:
    //
    // - Case-mismatched calls on case-insensitive filesystems
    //   (Windows / macOS HFS+) where `plugins/Foo/` and
    //   `plugins/foo/` resolve to the same dir but would produce
    //   distinct `app_setting` rows (`plugin.Foo.enabled` vs
    //   `plugin.foo.enabled`) and distinct lock map entries.
    // - Corrupt / unparsable manifests (no row for a plugin the
    //   host can't actually load).
    // - Dir-vs-manifest id drift (mirrors the runtime's load-time
    //   guard from Phase 2b).
    let id_for_blocking = plugin_id.clone();
    let manifest_ok = tokio::task::spawn_blocking(move || {
        let manifest_path = plugin_dir.join("manifest.toml");
        Manifest::load_from_path(&manifest_path)
            .map(|m| m.plugin.id == id_for_blocking)
            .unwrap_or(false)
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    if !manifest_ok {
        return Err(AppError::Other(format!(
            "plugin not installed (or manifest id mismatch): {plugin_id}"
        )));
    }

    // Full column set — `app_setting.value_type` and `updated_at`
    // are NOT NULL with a `CHECK` constraint on the type tag (see
    // `migrations/app/20260411120000_initial.sql`), so a shorter
    // INSERT would crash on the NOT NULL guard. Same UPSERT shape
    // every other writer in the workspace uses (`backup.rs` etc.).
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            value_type = excluded.value_type,
            updated_at = excluded.updated_at",
    )
    .bind(enabled_key(&plugin_id))
    .bind(if enabled { "true" } else { "false" })
    .bind(now)
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
/// Refuses bundled plugins ([`is_bundled_plugin`]) — the boot-time
/// extractor would re-seed them on next launch, so an "uninstall"
/// would silently roll itself back. Bundled plugins must be turned
/// off via [`set_plugin_enabled`] instead; the frontend hides the
/// uninstall button for these and points the user at the toggle.
///
/// When the audio engine is currently playing a non-library URL
/// (sentinel `current_track_id < 0`, set by `player_play_url` for
/// every plugin-minted stream), the engine is stopped before the
/// install dir is wiped. The plugin id isn't currently round-tripped
/// to the engine, so this stops any URL-based stream — accepted
/// trade-off vs leaving an orphan stream playing whose source view
/// has just been deleted.
///
/// Frontend must confirm with the user before invoking — the
/// command itself takes no "are you sure" parameter, on the same
/// principle as `delete_profile`.
#[tauri::command]
pub async fn uninstall_plugin(
    state: State<'_, AppState>,
    engine: State<'_, Arc<AudioEngine>>,
    plugin_id: String,
) -> AppResult<()> {
    // Character-class gate FIRST: rejects case-variant input
    // before any lock entry is created. See doc on
    // `validate_plugin_id_chars`.
    validate_plugin_id_chars(&plugin_id)?;

    // Bundled plugins live inside the installer and are re-seeded at
    // every boot by `ensure_bundled_plugins`. Allowing the uninstall
    // would create a one-launch ghost state — the user sees the
    // plugin disappear, restarts the app, and it's back. Refuse so
    // the contract is honest; the UI mirrors this by hiding the
    // button entirely on bundled rows.
    if is_bundled_plugin(&plugin_id) {
        return Err(AppError::Other(format!(
            "plugin {plugin_id} is bundled with WaveFlow and cannot be uninstalled (disable it instead)"
        )));
    }

    let paths = state.paths.plugin_paths();
    let install_dir = paths
        .plugin_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;
    let state_dir = paths
        .state_dir(&plugin_id)
        .map_err(|e| AppError::Other(format!("invalid plugin id: {e}")))?;

    // Serialise against `set_plugin_enabled` for the same id. Held
    // through both the FS removal AND the DELETE so the toggle
    // can never sneak in a new row between the dir-rm and the
    // setting-drop.
    let _guard = lock_plugin(&state, &plugin_id).await;

    // Manifest id pin (asymmetric vs `set_plugin_enabled`): refuse
    // to remove a tree whose manifest declares a DIFFERENT id, but
    // allow cleanup when the manifest is simply MISSING. On
    // case-insensitive filesystems `uninstall_plugin("Foo")` would
    // otherwise wipe `plugins/foo/` while leaving the
    // `plugin.foo.enabled` row intact, since our DELETE keys on
    // the param. The mismatch attack only matters when a manifest
    // IS present with another id — a missing manifest can't carry
    // a mismatch and refusing it would orphan the user's
    // `plugin-data/<id>/` + `app_setting` row forever (post-crash
    // leftover, half-installed plugin, manual `rm` of the install
    // dir). Other parse errors are still rejected because a corrupt
    // manifest could be a deliberately malformed file.
    let id_for_blocking = plugin_id.clone();
    let manifest_install_dir = install_dir.clone();
    let manifest_ok = tokio::task::spawn_blocking(move || {
        let manifest_path = manifest_install_dir.join("manifest.toml");
        match Manifest::load_from_path(&manifest_path) {
            Ok(m) => m.plugin.id == id_for_blocking,
            Err(ManifestError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        }
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    if !manifest_ok {
        return Err(AppError::Other(format!(
            "plugin manifest present but id mismatch (or corrupt): {plugin_id}"
        )));
    }

    // Stop any URL-based stream before we wipe the install dir.
    // `player_play_url` mints a strictly-negative `track_id` for
    // every HTTP stream it dispatches (radio + plugin-sourced),
    // and that id is currently the only signal that the engine is
    // serving something the local library can't anchor to. The
    // plugin id isn't round-tripped to the engine, so we can't
    // narrow this to "the URL was minted by THIS plugin" — accepted
    // trade-off vs orphaning a stream whose owner has just been
    // uninstalled. A library track is left alone.
    if engine.shared().current_track_id.load(AtomicOrdering::Relaxed) < 0 {
        tracing::info!(plugin_id, "stopping active URL stream before uninstall");
        if let Err(e) = engine.send(AudioCmd::Stop) {
            tracing::warn!(plugin_id, %e, "failed to stop engine; proceeding with uninstall");
        }
    }

    // Remove install + state on a blocking thread — `remove_dir_all`
    // on a multi-MB plugin tree (e.g. Web Radio embedding a ~10 MB
    // SQLite) can stretch into double-digit milliseconds and would
    // otherwise tie up a tokio worker. We don't pre-check
    // `exists()` (TOCTOU window between the check and the
    // syscall); `NotFound` on the rename itself is treated as
    // success since "user wanted it gone, it's already gone" is
    // the same end state.
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        match fs::remove_dir_all(&install_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AppError::Io(e)),
        }
        match fs::remove_dir_all(&state_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AppError::Io(e)),
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
async fn source_preamble(
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
            .map_err(|e| AppError::Other(format!("plugin {plugin_id}: {e}")))
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
            .map_err(|e| AppError::Other(format!("plugin {plugin_id}: {e}")))
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
            .map_err(|e| AppError::Other(format!("plugin {plugin_id}: {e}")))
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;
    Ok(url)
}

