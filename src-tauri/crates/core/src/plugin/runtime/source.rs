//! Calls into a `waveflow:source/provider` plugin.

use wasmtime::Store;

use crate::plugin::PluginPaths;

use super::{HostCtx, PluginRuntime, RuntimeError};

// ----- source-v1 invocation helpers ---------------------------------------
//
// Loads + instantiates + calls the `waveflow:source/provider`
// exports. Returns owned DTOs so the caller (Tauri commands, future
// waveflow-server handlers) doesn't have to depend on wasmtime
// itself — every plumbing crate the host needs lives behind these
// three free functions.

/// Owned mirror of `waveflow:source/provider/entry` — what the
/// plugin returns from `list-entries`.
#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub label: String,
    pub query: String,
    pub icon_url: Option<String>,
}

/// Owned mirror of `waveflow:source/provider/track` — what the
/// plugin returns from `resolve`.
#[derive(Debug, Clone)]
pub struct SourceTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: u32,
    pub artwork_url: Option<String>,
    pub icy_url: Option<String>,
}

/// Errors specific to the source-invocation surface. Distinct from
/// [`RuntimeError`] so callers can tell a plugin-side `Err` from a
/// host-side trap / load failure.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("runtime: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("instantiate: {0}")]
    Instantiate(String),
    /// The whole cause chain (`{e:#}`), not `to_string()`: the latter is
    /// only wasmtime's "error while executing at wasm backtrace" header,
    /// which dropped the reason — fuel, a guest panic, an epoch interrupt
    /// — and left a trap in a tester's log unexplained (#733).
    #[error("trap: {0}")]
    Trap(String),
    #[error("plugin: {0}")]
    Plugin(String),
}

impl SourceError {
    /// `true` when the plugin never started. A guest that returns an
    /// error or traps has run, and may simply have had nothing for
    /// this track; a load failure repeats on every call until the
    /// file changes, and deserves to be said louder.
    pub fn is_load_failure(&self) -> bool {
        matches!(self, Self::Runtime(_))
    }

    /// Same contract as [`RuntimeError::detail`]: the whole cause
    /// chain, not just the layer nearest the caller. A log line that
    /// prints this error with `%err` stops at "runtime: wasmtime:
    /// failed to parse WebAssembly module" and drops the sentence that
    /// says what to do about it.
    pub fn detail(&self) -> String {
        match self {
            Self::Runtime(err) => format!("runtime: {}", err.detail()),
            other => other.to_string(),
        }
    }
}

define_instantiate!(instantiate_source, crate::plugin::bindings::source::Plugin);

/// Call the guest's `list-entries`.
pub fn source_list_entries(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
) -> Result<Vec<SourceEntry>, SourceError> {
    let (mut store, plugin) = instantiate_source(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_source_provider()
        .call_list_entries(&mut store)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(entries) => Ok(entries
            .into_iter()
            .map(|e| SourceEntry {
                label: e.label,
                query: e.query,
                icon_url: e.icon_url,
            })
            .collect()),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

/// Call the guest's `resolve(query)`.
pub fn source_resolve(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    query: &str,
) -> Result<Vec<SourceTrack>, SourceError> {
    let (mut store, plugin) = instantiate_source(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_source_provider()
        .call_resolve(&mut store, query)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(tracks) => Ok(tracks
            .into_iter()
            .map(|t| SourceTrack {
                id: t.id,
                title: t.title,
                artist: t.artist,
                album: t.album,
                duration_ms: t.duration_ms,
                artwork_url: t.artwork_url,
                icy_url: t.icy_url,
            })
            .collect()),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

/// Call the guest's `stream-url(track-id)`.
pub fn source_stream_url(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    track_id: &str,
) -> Result<String, SourceError> {
    let (mut store, plugin) = instantiate_source(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_source_provider()
        .call_stream_url(&mut store, track_id)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    result.map_err(SourceError::Plugin)
}
