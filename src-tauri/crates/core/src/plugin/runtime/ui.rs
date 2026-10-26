//! Calls into a `waveflow:ui/extension` plugin.

use wasmtime::Store;

use crate::plugin::PluginPaths;

use super::{HostCtx, PluginRuntime, RuntimeError};

// ----- ui-v1 invocation helpers -------------------------------------------
//
// Instantiates + calls the `waveflow:ui/extension` exports. A UI
// plugin describes views as JSON descriptors; these helpers return
// the raw descriptor strings (+ the mount-point) as owned DTOs so the
// Tauri command layer never depends on wasmtime. Each call re-
// instantiates the component from scratch (load → linker → store →
// instantiate), same as the source / metadata helpers — a UI
// interaction is a human-paced click, not a hot loop, so the
// instantiate cost is irrelevant and a fresh store keeps per-call
// state (the artist snapshot) from leaking across invocations.

/// Redacted artist row handed to a `ui` plugin via
/// `waveflow:host/library.list-artists`. Names + counts + an opaque
/// id — see [`HostCtx::library_artists`]. Owned so the host / command
/// layer builds the snapshot without depending on the bindgen types.
#[derive(Debug, Clone)]
pub struct LibraryArtist {
    pub id: u64,
    pub name: String,
    pub track_count: u32,
}

/// Owned mirror of `waveflow:ui/extension/mount-point` — where a UI
/// plugin plants its sidebar entry.
#[derive(Debug, Clone)]
pub struct UiMountPoint {
    pub sidebar_label: String,
    pub sidebar_icon: Option<String>,
    pub initial_path: String,
}

/// Errors specific to the ui-invocation surface. Mirrors
/// [`SourceError`]'s split so callers can tell a plugin-side `Err`
/// (shown as a view-level error banner) from a host-side trap / load
/// failure.
#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("runtime: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("instantiate: {0}")]
    Instantiate(String),
    #[error("trap: {0}")]
    Trap(String),
    #[error("plugin: {0}")]
    Plugin(String),
}

impl UiError {
    /// See [`SourceError::detail`].
    pub fn detail(&self) -> String {
        match self {
            Self::Runtime(err) => format!("runtime: {}", err.detail()),
            other => other.to_string(),
        }
    }
}

fn instantiate_ui(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    library_artists: Vec<LibraryArtist>,
) -> Result<(Store<HostCtx>, crate::plugin::bindings::ui::Plugin), UiError> {
    let loaded = runtime.load_plugin(paths, plugin_id)?;
    let linker = runtime.build_linker()?;
    let mut store =
        runtime.new_store_for_plugin_with_library_artists(&loaded, paths, library_artists)?;
    let plugin =
        crate::plugin::bindings::ui::Plugin::instantiate(&mut store, &loaded.component, &linker)
            .map_err(|e| UiError::Instantiate(e.to_string()))?;
    Ok((store, plugin))
}

/// Call the guest's `manifest()` — sidebar registration metadata.
pub fn ui_manifest(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
) -> Result<UiMountPoint, UiError> {
    let (mut store, plugin) = instantiate_ui(runtime, paths, plugin_id, Vec::new())?;
    let mp = plugin
        .waveflow_ui_extension()
        .call_manifest(&mut store)
        .map_err(|e| UiError::Trap(format!("{e:#}")))?;
    Ok(UiMountPoint {
        sidebar_label: mp.sidebar_label,
        sidebar_icon: mp.sidebar_icon,
        initial_path: mp.initial_path,
    })
}

/// Call the guest's `render(path)` — returns the raw JSON view
/// descriptor. `library_artists` is the redacted snapshot the host
/// loaded for this call (empty when the plugin lacks the permission).
pub fn ui_render(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    path: &str,
    library_artists: Vec<LibraryArtist>,
) -> Result<String, UiError> {
    let (mut store, plugin) = instantiate_ui(runtime, paths, plugin_id, library_artists)?;
    plugin
        .waveflow_ui_extension()
        .call_render(&mut store, path)
        .map_err(|e| UiError::Trap(format!("{e:#}")))?
        .map_err(UiError::Plugin)
}

/// Call the guest's `on-event(event, payload)` — returns the NEXT raw
/// JSON view descriptor that replaces the current view.
pub fn ui_event(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    event: &str,
    payload: &str,
    library_artists: Vec<LibraryArtist>,
) -> Result<String, UiError> {
    let (mut store, plugin) = instantiate_ui(runtime, paths, plugin_id, library_artists)?;
    plugin
        .waveflow_ui_extension()
        .call_on_event(&mut store, event, payload)
        .map_err(|e| UiError::Trap(format!("{e:#}")))?
        .map_err(UiError::Plugin)
}
