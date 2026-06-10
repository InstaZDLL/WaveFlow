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

use std::path::{Component, Path, PathBuf};

/// Root layout: `<app-data>/waveflow/plugins/<plugin-id>/manifest.toml
/// + <plugin-id>/assets/*`. One directory per installed plugin.
#[derive(Debug, Clone)]
pub struct PluginPaths {
    /// `<app-data>/waveflow/plugins/`.
    pub root: PathBuf,
}

/// `plugin_id` failed the path-shape check inside [`PluginPaths`].
/// Callers should treat this the same as "manifest is invalid" —
/// refuse to load the plugin.
#[derive(Debug, thiserror::Error)]
#[error("invalid plugin id: {0:?}")]
pub struct InvalidPluginId(pub String);

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

    /// Sanitise a plugin id so it can never escape `self.root`. The
    /// manifest validator already restricts ids to `[a-z0-9-]+`,
    /// but `PluginPaths` is the last line of defence: anything
    /// that walks the id through `Path::join` without checking
    /// would let an absolute id (`/etc/passwd`, `C:\Windows`) or
    /// one carrying `..` segments land outside the plugins
    /// directory. We require the input to decompose into exactly
    /// one `Component::Normal` whose string form is byte-for-byte
    /// what was passed in — that rules out separators, parent
    /// segments, drive letters, and empty strings.
    fn sanitise_id(plugin_id: &str) -> Result<&str, InvalidPluginId> {
        if plugin_id.is_empty() {
            return Err(InvalidPluginId(plugin_id.into()));
        }
        let path = Path::new(plugin_id);
        let mut components = path.components();
        let Some(first) = components.next() else {
            return Err(InvalidPluginId(plugin_id.into()));
        };
        if components.next().is_some() {
            return Err(InvalidPluginId(plugin_id.into()));
        }
        match first {
            Component::Normal(name) if name.to_str() == Some(plugin_id) => Ok(plugin_id),
            _ => Err(InvalidPluginId(plugin_id.into())),
        }
    }

    /// Path of one plugin's root directory. Returns
    /// [`InvalidPluginId`] when `plugin_id` would escape
    /// `self.root` (absolute path, `..` segment, embedded
    /// separator).
    pub fn plugin_dir(&self, plugin_id: &str) -> Result<PathBuf, InvalidPluginId> {
        Self::sanitise_id(plugin_id).map(|id| self.root.join(id))
    }

    /// Path to `manifest.toml` for one plugin.
    pub fn manifest_path(&self, plugin_id: &str) -> Result<PathBuf, InvalidPluginId> {
        self.plugin_dir(plugin_id)
            .map(|dir| dir.join("manifest.toml"))
    }

    /// Path to the compiled WASM component for one plugin.
    pub fn wasm_path(&self, plugin_id: &str) -> Result<PathBuf, InvalidPluginId> {
        self.plugin_dir(plugin_id)
            .map(|dir| dir.join("plugin.wasm"))
    }

    /// Path to the bundled asset directory for one plugin.
    /// Empty / missing is fine — plugins that don't ship assets
    /// just leave the table out of their manifest.
    pub fn assets_dir(&self, plugin_id: &str) -> Result<PathBuf, InvalidPluginId> {
        self.plugin_dir(plugin_id).map(|dir| dir.join("assets"))
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
            paths.manifest_path("web-radio").unwrap(),
            Path::new("/tmp/waveflow/plugins/web-radio/manifest.toml")
        );
        assert_eq!(
            paths.assets_dir("web-radio").unwrap(),
            Path::new("/tmp/waveflow/plugins/web-radio/assets")
        );
        assert_eq!(
            paths.wasm_path("web-radio").unwrap(),
            Path::new("/tmp/waveflow/plugins/web-radio/plugin.wasm")
        );
    }

    #[test]
    fn plugin_dir_rejects_absolute_id() {
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
        // Unix-style absolute id — would otherwise escape into /etc.
        assert!(paths.plugin_dir("/etc/passwd").is_err());
    }

    #[test]
    fn plugin_dir_rejects_parent_segment() {
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
        // `..` would walk up out of the plugins directory.
        assert!(paths.plugin_dir("../escape").is_err());
        assert!(paths.plugin_dir("..").is_err());
    }

    #[test]
    fn plugin_dir_rejects_embedded_separator() {
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
        // Even without `..`, an embedded `/` walks into a sub-tree
        // the host wasn't expecting (e.g. mounting an asset path as
        // a plugin id).
        assert!(paths.plugin_dir("web-radio/subdir").is_err());
    }

    #[test]
    fn plugin_dir_rejects_empty() {
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
        assert!(paths.plugin_dir("").is_err());
    }
}
