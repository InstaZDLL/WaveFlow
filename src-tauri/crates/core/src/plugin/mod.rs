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
pub mod bindings;
pub mod host_impl;
pub mod manifest;
pub mod runtime;

use std::path::{Component, Path, PathBuf};

/// Hardcoded list of plugins WaveFlow ships bundled in the
/// installer. Lives in core so [`PluginPaths`] can route bundled
/// ids to the resource dir without round-tripping back through the
/// app layer.
///
/// Phase 5 will replace this with a manifest-driven discovery
/// (loop over `<resource_dir>/plugins/*/manifest.toml`) so adding
/// a plugin to the bundle doesn't require touching app code; for
/// now the static list keeps the diff focused.
pub const BUNDLED_PLUGINS: &[&str] = &["web-radio"];

/// True when `plugin_id` is a first-party plugin shipped inside the
/// installer. Used by [`PluginPaths`] to route path resolution to
/// the resource dir, by the app to refuse `uninstall_plugin` (the
/// installer would re-seed the tree on next launch so the uninstall
/// reads as a bug), and by the UI to render a "bundled" badge.
pub fn is_bundled_plugin(plugin_id: &str) -> bool {
    BUNDLED_PLUGINS.contains(&plugin_id)
}

/// Three parallel roots resolved by the host:
///
/// - `bundled_root` (optional, `<resource_dir>/plugins/`) — read-only
///   install root for plugins shipped inside the application
///   installer. Resolved from `BaseDirectory::Resource` on app side;
///   `None` in core-only tests or when no bundled tree is present.
///   When [`is_bundled_plugin`] returns true, path resolution
///   targets this root instead of `plugins_root`.
/// - `plugins_root` (`<app-data>/waveflow/plugins/`) — sideloaded
///   install dir, writable. One subdirectory per sideloaded plugin
///   holding `manifest.toml`, `plugin.wasm`, and the `assets/`
///   subtree. Bundled plugins are NEVER copied here (was the case
///   pre-1.5.1 via `ensure_bundled_plugins`; that path wasted
///   ~150 KB per bundled plugin per install and confused users who
///   went folder spelunking — see issue #280). The cleanup pass at
///   boot drops any leftover bundled entry from a 1.5.0 → 1.5.1
///   upgrade.
/// - `data_root` (`<app-data>/waveflow/plugin-data/`) — per-user
///   scratch store the host hands to the plugin via
///   `waveflow:host/storage.{read,write}-state`. Always in the
///   writable app-data tree, regardless of where the .wasm itself
///   lives — bundled plugins still write state to the user's data
///   dir.
#[derive(Debug, Clone)]
pub struct PluginPaths {
    /// Optional read-only install root for bundled plugins (resource
    /// dir, e.g. `<install>/plugins/` on Windows NSIS, `/usr/lib/WaveFlow/plugins/`
    /// on Linux). When `Some`, [`is_bundled_plugin`] ids resolve here.
    pub bundled_root: Option<PathBuf>,
    /// `<app-data>/waveflow/plugins/` — sideloaded install root.
    pub plugins_root: PathBuf,
    /// `<app-data>/waveflow/plugin-data/` — scratch root, one dir per plugin.
    pub data_root: PathBuf,
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
    /// subdirectories so every plugin shares one layout.
    pub fn from_app_data(app_data_dir: &Path) -> Self {
        Self {
            bundled_root: None,
            plugins_root: app_data_dir.join("plugins"),
            data_root: app_data_dir.join("plugin-data"),
        }
    }

    /// Inject the resolved bundled plugins root (typically
    /// `<resource_dir>/plugins/`). Consumed-then-returned builder
    /// style so the app can chain it off `from_app_data` at startup
    /// without rebuilding the struct. A `None` argument is accepted
    /// and clears the field, which keeps the helper symmetric for
    /// tests that want to opt-out.
    pub fn with_bundled_root(mut self, bundled_root: Option<PathBuf>) -> Self {
        self.bundled_root = bundled_root;
        self
    }

    /// Pick the install root for `plugin_id`. Bundled ids resolve
    /// against [`Self::bundled_root`] when present, sideloaded ids
    /// against [`Self::plugins_root`]. The fallback (`bundled_root`
    /// is `None`) lets a bundled id still resolve under
    /// `plugins_root` so unit tests + core-only contexts keep
    /// working without a resource tree on disk.
    fn install_root_for(&self, plugin_id: &str) -> &Path {
        if is_bundled_plugin(plugin_id) {
            if let Some(root) = &self.bundled_root {
                return root;
            }
        }
        &self.plugins_root
    }

    /// Sanitise a plugin id so it can never escape `self.plugins_root`
    /// or `self.data_root`. The manifest validator already restricts
    /// ids to `[a-z0-9-]+`, but `PluginPaths` is the last line of
    /// defence: anything that walks the id through `Path::join`
    /// without checking would let an absolute id (`/etc/passwd`,
    /// `C:\Windows`) or one carrying `..` segments land outside the
    /// plugin tree. We require the input to decompose into exactly
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

    /// Path of one plugin's install directory. Bundled ids resolve
    /// under [`Self::bundled_root`] (read-only, populated from the
    /// resource dir), sideloaded ids under [`Self::plugins_root`]
    /// (writable, in the app-data tree). Returns [`InvalidPluginId`]
    /// when `plugin_id` would escape the chosen root (absolute path,
    /// `..` segment, embedded separator).
    pub fn plugin_dir(&self, plugin_id: &str) -> Result<PathBuf, InvalidPluginId> {
        Self::sanitise_id(plugin_id).map(|id| self.install_root_for(id).join(id))
    }

    /// Path of one plugin's per-user scratch directory under
    /// `data_root`. Same id sanitisation contract as
    /// [`Self::plugin_dir`]. Phase 2b's `waveflow:host/storage.{read,write}-state`
    /// reads + writes inside this tree (one file per state key).
    /// The helper itself is non-mutating; the directory is created
    /// lazily by [`crate::plugin::host_impl::StateStore::write`] on
    /// the first call, so callers don't need to `create_dir_all` it
    /// up front (a plugin that never writes leaves no trace on
    /// disk).
    pub fn state_dir(&self, plugin_id: &str) -> Result<PathBuf, InvalidPluginId> {
        Self::sanitise_id(plugin_id).map(|id| self.data_root.join(id))
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
        assert_eq!(paths.plugins_root, Path::new("/tmp/waveflow/plugins"));
        assert_eq!(paths.data_root, Path::new("/tmp/waveflow/plugin-data"));
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
        assert_eq!(
            paths.state_dir("web-radio").unwrap(),
            Path::new("/tmp/waveflow/plugin-data/web-radio")
        );
    }

    #[test]
    fn state_dir_shares_sanitisation_with_plugin_dir() {
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
        assert!(paths.state_dir("/etc/passwd").is_err());
        assert!(paths.state_dir("..").is_err());
        assert!(paths.state_dir("web-radio/subdir").is_err());
        assert!(paths.state_dir("").is_err());
    }

    #[test]
    fn bundled_plugin_routes_to_bundled_root() {
        // When `bundled_root` is set, a bundled id (`web-radio` per
        // [`BUNDLED_PLUGINS`]) resolves under it for install + manifest
        // + wasm + assets — but NOT for state_dir, which always stays
        // in the writable app-data tree so user state survives an
        // install dir refresh.
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"))
            .with_bundled_root(Some(PathBuf::from("/opt/waveflow/resources/plugins")));
        assert_eq!(
            paths.plugin_dir("web-radio").unwrap(),
            Path::new("/opt/waveflow/resources/plugins/web-radio")
        );
        assert_eq!(
            paths.manifest_path("web-radio").unwrap(),
            Path::new("/opt/waveflow/resources/plugins/web-radio/manifest.toml")
        );
        assert_eq!(
            paths.wasm_path("web-radio").unwrap(),
            Path::new("/opt/waveflow/resources/plugins/web-radio/plugin.wasm")
        );
        assert_eq!(
            paths.assets_dir("web-radio").unwrap(),
            Path::new("/opt/waveflow/resources/plugins/web-radio/assets")
        );
        // State lives in the writable tree even for bundled plugins.
        assert_eq!(
            paths.state_dir("web-radio").unwrap(),
            Path::new("/tmp/waveflow/plugin-data/web-radio")
        );
    }

    #[test]
    fn sideloaded_plugin_stays_in_plugins_root_even_with_bundled_set() {
        // Non-bundled ids always resolve under `plugins_root` (the
        // sideloaded tree), regardless of whether a `bundled_root`
        // was injected. This is what keeps user-installed plugins
        // working alongside first-party bundled ones.
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"))
            .with_bundled_root(Some(PathBuf::from("/opt/waveflow/resources/plugins")));
        assert_eq!(
            paths.plugin_dir("my-custom").unwrap(),
            Path::new("/tmp/waveflow/plugins/my-custom")
        );
    }

    #[test]
    fn bundled_plugin_falls_back_to_plugins_root_without_bundled_set() {
        // Tests + core-only contexts that never resolve a resource
        // dir must still be able to call `plugin_dir("web-radio")`
        // and get a deterministic path back — fall back to
        // `plugins_root` so existing fixtures keep working.
        let paths = PluginPaths::from_app_data(Path::new("/tmp/waveflow"));
        assert!(paths.bundled_root.is_none());
        assert_eq!(
            paths.plugin_dir("web-radio").unwrap(),
            Path::new("/tmp/waveflow/plugins/web-radio")
        );
    }

    #[test]
    fn is_bundled_plugin_only_matches_known_ids() {
        assert!(is_bundled_plugin("web-radio"));
        assert!(!is_bundled_plugin("web-Radio")); // case-sensitive
        assert!(!is_bundled_plugin("not-bundled"));
        assert!(!is_bundled_plugin(""));
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
