use crate::{
    error::{AppError, AppResult},
    paths::AppPaths,
};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use waveflow_core::plugin::PluginPaths;

impl AppPaths {
    /// Resolve all paths from a Tauri [`AppHandle`].
    ///
    /// Does **not** create any directories on disk. Call [`Self::ensure_dirs`]
    /// after construction to materialize the layout.
    ///
    /// Caches resolve under the same root; [`Self::with_cache_root`]
    /// moves them afterwards, once `app.db` has been opened and the
    /// stored choice read.
    pub fn from_handle(handle: &AppHandle) -> AppResult<Self> {
        let data_dir = handle
            .path()
            .app_data_dir()
            .map_err(|_| AppError::MissingAppDataDir)?;

        // Bundled plugin resources live next to the binary, resolved
        // via Tauri's `BaseDirectory::Resource`. A failure here isn't
        // fatal — `PluginPaths::install_root_for` falls back to the
        // writable app-data tree when `bundled_root` is `None`, so
        // the only consequence in dev builds or broken installs is
        // that bundled plugins resolve under `<app-data>/plugins/`
        // (matching pre-1.5.1 behaviour). We log the failure so a
        // mispackaged installer surfaces visibly in tracing.
        let bundled_plugins_dir = match handle.path().resolve("plugins", BaseDirectory::Resource) {
            Ok(path) if path.exists() && path.is_dir() => Some(path),
            Ok(path) => {
                tracing::warn!(
                    path = %path.display(),
                    "bundled plugins resource dir not found; bundled plugins will fall back to app-data tree",
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    %e,
                    "bundled plugins resource dir not resolvable; bundled plugins will fall back to app-data tree",
                );
                None
            }
        };

        Ok(Self::from_root(
            data_dir.join("waveflow"),
            bundled_plugins_dir,
        ))
    }

    /// Plugin install + scratch roots, in [`PluginPaths`] form so the
    /// runtime can pass it straight into `PluginRuntime::load_plugin`
    /// and `new_store_for_plugin`. Layout:
    ///
    /// ```text
    /// <resource_dir>/plugins/<id>/      (bundled install dir, read-only — when resolvable)
    /// <root>/plugins/<plugin-id>/       (sideloaded install dir, writable)
    /// <root>/plugin-data/<plugin-id>/   (per-user scratch, written by host imports)
    /// ```
    ///
    /// Bundled ids resolve under `<resource_dir>/plugins/` (the
    /// installer's read-only payload), sideloaded ids under
    /// `<root>/plugins/` (writable app-data). State writes always
    /// land in `<root>/plugin-data/` regardless of where the .wasm
    /// lives — bundled plugins still need a writable scratch dir.
    pub fn plugin_paths(&self) -> PluginPaths {
        PluginPaths::from_app_data(&self.root).with_bundled_root(self.bundled_plugins_dir.clone())
    }
}
