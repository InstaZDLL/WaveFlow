//! Calls into a `waveflow:canvas/provider` plugin.

use wasmtime::Store;

use crate::plugin::PluginPaths;

use super::{HostCtx, PluginRuntime, SourceError};

// ----- canvas-v1 invocation helpers ---------------------------------------
//
// Instantiates + calls the `waveflow:canvas/provider` export. A canvas
// plugin resolves a per-track looping video URL (issue #473). Reuses
// [`SourceError`]'s Instantiate / Trap / Plugin split — world-agnostic,
// same as the metadata helpers. No library snapshot (canvas providers
// import only http/log/storage/config), so it uses the plain
// `new_store_for_plugin`.

/// Owned mirror of `waveflow:canvas/provider/canvas` — a resolved
/// per-track Canvas.
#[derive(Debug, Clone)]
pub struct ProviderCanvas {
    /// Directly-playable looping mp4 URL.
    pub url: String,
    /// Opaque source entity id (e.g. the resolved external track id),
    /// if the plugin supplied one. The host does not interpret it.
    pub entity_id: Option<String>,
}

define_instantiate!(instantiate_canvas, crate::plugin::bindings::canvas::Plugin);

/// Call the guest's `track-canvas(artist, title, album?, duration-ms?)`.
/// `Ok(None)` = the plugin has no Canvas for this track (the host then
/// falls back to motion artwork → slideshow → the static cover).
pub fn canvas_track_canvas(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    artist: &str,
    title: &str,
    album: Option<&str>,
    duration_ms: Option<u32>,
) -> Result<Option<ProviderCanvas>, SourceError> {
    let (mut store, plugin) = instantiate_canvas(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_canvas_provider()
        .call_track_canvas(&mut store, artist, title, album, duration_ms)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(Some(c)) => Ok(Some(ProviderCanvas {
            url: c.url,
            entity_id: c.entity_id,
        })),
        Ok(None) => Ok(None),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}
