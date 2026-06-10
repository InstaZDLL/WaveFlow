//! Plugin host (Phase 1: skeleton — manifest + sidecar assets).
//!
//! This module hosts WaveFlow's plugin protocol on the core side so
//! both the desktop app and the future `waveflow-server` can run the
//! same plugins. It is intentionally small at Phase 1:
//!
//! - [`manifest`] parses `manifest.toml` and validates the declared
//!   world + permissions against the SDK's catalog.
//! - [`assets`] resolves sidecar files declared in the manifest's
//!   `[[assets]]` table. Plugins call into the host's
//!   `waveflow:host/storage.read-asset` import which lands here.
//! - [`PluginPaths`] resolves where plugins live on disk:
//!   `<app-data>/waveflow/plugins/<plugin-id>/`. The same convention
//!   on every OS (Tauri's app-data dir varies).
//!
//! Phase 2 will wire `wasmtime::component` for actual execution. This
//! phase intentionally ships without `wasmtime` to keep the desktop
//! bundle size unchanged for the bug-fix release.

pub mod assets;
pub mod manifest;

use std::path::{Path, PathBuf};

/// Root layout: `<app-data>/waveflow/plugins/<plugin-id>/manifest.toml
/// + <plugin-id>/assets/*`. One directory per installed plugin.
#[derive(Debug, Clone)]
pub struct PluginPaths {
    /// `<app-data>/waveflow/plugins/`.
    pub root: PathBuf,
}

impl PluginPaths {
    /// Build a [`PluginPaths`] anchored at the given app-data dir.
    /// The host caller (`waveflow::AppPaths`) owns the OS-specific
    /// resolution; this helper just appends the canonical
    /// subdirectory so every plugin shares one layout.
    pub fn from_app_data(app_data_dir: &Path) -> Self {
        Self {
            root: app_data_dir.join("plugins"),
        }
    }

    /// Path of one plugin's root directory.
    pub fn plugin_dir(&self, plugin_id: &str) -> PathBuf {
        self.root.join(plugin_id)
    }

    /// Path to `manifest.toml` for one plugin.
    pub fn manifest_path(&self, plugin_id: &str) -> PathBuf {
        self.plugin_dir(plugin_id).join("manifest.toml")
    }

    /// Path to the compiled WASM component for one plugin.
    pub fn wasm_path(&self, plugin_id: &str) -> PathBuf {
        self.plugin_dir(plugin_id).join("plugin.wasm")
    }

    /// Path to the bundled asset directory for one plugin.
    /// Empty / missing is fine — plugins that don't ship assets
    /// just leave the table out of their manifest.
    pub fn assets_dir(&self, plugin_id: &str) -> PathBuf {
        self.plugin_dir(plugin_id).join("assets")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_paths_compose_under_root() {
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
        assert_eq!(paths.root, Path::new("/tmp/waveflow/plugins"));
        assert_eq!(
            paths.manifest_path("web-radio"),
            Path::new("/tmp/waveflow/plugins/web-radio/manifest.toml")
        );
        assert_eq!(
            paths.assets_dir("web-radio"),
            Path::new("/tmp/waveflow/plugins/web-radio/assets")
        );
        assert_eq!(
            paths.wasm_path("web-radio"),
            Path::new("/tmp/waveflow/plugins/web-radio/plugin.wasm")
        );
    }
}
