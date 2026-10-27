//! Listing the installed plugins, enabling and uninstalling one, and
//! opening the plugins folder.

use std::fs;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;

use tauri::State;
use waveflow_core::plugin::manifest::{Manifest, ManifestError};

use crate::audio::{AudioCmd, AudioEngine};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use waveflow_core::plugin::is_bundled_plugin;

use super::*;

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
pub(crate) fn validate_plugin_id_chars(plugin_id: &str) -> AppResult<()> {
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

pub(super) async fn read_enabled(app_db: &sqlx::SqlitePool, plugin_id: &str) -> AppResult<bool> {
    // Missing row = enabled. We don't pre-populate on install so a
    // brand-new plugin always starts on; the user has to flip the
    // toggle to opt-out.
    let row: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
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
    let manifests = tokio::task::spawn_blocking(
        move || -> AppResult<Vec<(String, Manifest, Option<PluginFailure>)>> {
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut out = Vec::new();

            // Bundled tree FIRST so its ids own the slot on any collision
            // with a stray sideloaded entry. `bundled_root` is optional
            // because dev builds without a bundle still need to list
            // sideloaded plugins.
            if let Some(bundled_root) = paths.bundled_root.as_deref() {
                for (id, manifest) in walk_install_root(bundled_root)? {
                    seen.insert(id.clone());
                    let probe = probe_wasm_format(&paths, &id);
                    out.push((id, manifest, probe));
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
                let probe = probe_wasm_format(&paths, &id);
                out.push((id, manifest, probe));
            }
            Ok(out)
        },
    )
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;

    let mut out = Vec::with_capacity(manifests.len());
    for (plugin_id, manifest, probe) in manifests {
        let enabled = read_enabled(&state.app_db, &plugin_id).await?;
        // What actually happened beats what the file looks like: a
        // component that compiles on one host and not on this one is
        // invisible to the probe, and the recorded failure carries the
        // message wasmtime gave.
        let failure = state
            .plugins
            .load_failure(&plugin_id)
            .map(PluginFailure::from)
            .or(probe);
        out.push(manifest_to_info(manifest, enabled, failure));
    }

    // Stable order so the frontend gets the same list across calls
    // without sorting itself.
    out.sort_by_key(|a| a.name.to_lowercase());
    Ok(out)
}

/// Ids of installed + enabled plugins whose declared world starts with
/// `world_prefix` (e.g. `"waveflow:metadata"`). Used by cross-world
/// dispatchers that fan a request out to every plugin of a kind — the
/// motion-artwork command asks every enabled metadata plugin. Bundled
/// ids win on collision, same as [`list_installed_plugins`].
pub(crate) async fn enabled_plugin_ids_for_world(
    state: &AppState,
    world: &str,
) -> AppResult<Vec<String>> {
    enabled_plugin_ids_matching(state, world, None).await
}

/// [`enabled_plugin_ids_for_world`], narrowed to the plugins whose
/// manifest says they answer `function` — so a lyrics lookup does not
/// instantiate an animated-cover plugin to hear it has nothing, and the
/// other way round. A manifest that names no function answers them all.
pub(crate) async fn enabled_plugin_ids_providing(
    state: &AppState,
    world: &str,
    function: &'static str,
) -> AppResult<Vec<String>> {
    enabled_plugin_ids_matching(state, world, Some(function)).await
}

async fn enabled_plugin_ids_matching(
    state: &AppState,
    world: &str,
    function: Option<&'static str>,
) -> AppResult<Vec<String>> {
    let paths = state.paths.plugin_paths();
    let wanted = world.to_string();
    let matches = move |m: &Manifest| {
        m.plugin.world == wanted && function.map_or(true, |f| m.plugin.provides(f))
    };
    let ids = tokio::task::spawn_blocking(move || -> AppResult<Vec<String>> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out = Vec::new();
        if let Some(bundled_root) = paths.bundled_root.as_deref() {
            for (id, m) in walk_install_root(bundled_root)? {
                if seen.insert(id.clone()) && matches(&m) {
                    out.push(id);
                }
            }
        }
        for (id, m) in walk_install_root(&paths.plugins_root)? {
            if !seen.contains(&id) && matches(&m) {
                out.push(id);
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;

    let mut enabled = Vec::with_capacity(ids.len());
    for id in ids {
        if read_enabled(&state.app_db, &id).await? {
            enabled.push(id);
        }
    }
    Ok(enabled)
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
    let probe_id = plugin_id.clone();
    let found =
        tokio::task::spawn_blocking(move || -> Option<(Manifest, Option<PluginFailure>)> {
            let manifest = Manifest::load_from_path(&manifest_path)
                .ok()
                .filter(|m| m.plugin.id == id_for_blocking)?;
            let probe = probe_wasm_format(&paths, &probe_id);
            Some((manifest, probe))
        })
        .await
        .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    match found {
        Some((manifest, probe)) => {
            let enabled = read_enabled(&state.app_db, &plugin_id).await?;
            let failure = state
                .plugins
                .load_failure(&plugin_id)
                .map(PluginFailure::from)
                .or(probe);
            Ok(Some(manifest_to_info(manifest, enabled, failure)))
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
    if engine
        .shared()
        .current_track_id
        .load(AtomicOrdering::Relaxed)
        < 0
    {
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

    state.plugins.forget_load_failure(&plugin_id);
    tracing::info!(plugin_id, "plugin uninstalled");
    Ok(())
}

/// Open the sideload directory in the system file manager.
///
/// The empty plugins list used to name the directory as
/// `<data>/waveflow/plugins/`, which resolves to `%APPDATA%`,
/// `~/.local/share` or `~/Library/Application Support` depending on the
/// platform and to nothing at all for the reader (issue #679). The app
/// knows the real path, so it opens it instead of describing it.
///
/// [`AppPaths::ensure_dirs`] already creates this root at boot, but a
/// user who deleted it — or an install whose boot-time creation failed
/// on a permission error since repaired — would otherwise get a silent
/// no-op from the opener. Creating on demand costs one syscall and
/// makes the button work whatever happened before it.
#[tauri::command]
pub async fn open_plugins_folder(state: State<'_, AppState>) -> AppResult<()> {
    let root = state.paths.plugin_paths().plugins_root;
    let root = tokio::task::spawn_blocking(move || -> AppResult<std::path::PathBuf> {
        fs::create_dir_all(&root)
            .map_err(|e| AppError::Other(format!("create {}: {e}", root.display())))?;
        Ok(root)
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))??;
    crate::external_open::open_path(root)
}
