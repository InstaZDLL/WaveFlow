use std::path::PathBuf;

use tauri::{AppHandle, Manager};
use waveflow_core::plugin::PluginPaths;

use crate::error::{AppError, AppResult};

/// Resolved filesystem paths for the application.
///
/// Layout (on Windows example, equivalent on macOS/Linux via Tauri's data dir):
///
/// ```text
/// <app_data>/waveflow/
/// ├── app.db                    (global registry + app settings)
/// ├── avatars/                  (shared profile avatars, hash-addressed)
/// ├── metadata_artwork/         (shared remote artwork cache, hash-addressed)
/// └── profiles/
///     └── <profile_id>/
///         ├── data.db           (per-profile database)
///         └── artwork/          (per-profile artwork cache)
/// ```
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    pub app_db: PathBuf,
    pub avatars_dir: PathBuf,
    pub metadata_artwork_dir: PathBuf,
    pub profiles_dir: PathBuf,
}

impl AppPaths {
    /// Resolve all paths from a Tauri [`AppHandle`].
    ///
    /// Does **not** create any directories on disk. Call [`Self::ensure_dirs`]
    /// after construction to materialize the layout.
    pub fn from_handle(handle: &AppHandle) -> AppResult<Self> {
        let data_dir = handle
            .path()
            .app_data_dir()
            .map_err(|_| AppError::MissingAppDataDir)?;

        let root = data_dir.join("waveflow");

        Ok(Self {
            app_db: root.join("app.db"),
            avatars_dir: root.join("avatars"),
            metadata_artwork_dir: root.join("metadata_artwork"),
            profiles_dir: root.join("profiles"),
            root,
        })
    }

    /// Create every directory that the application expects to exist.
    ///
    /// Individual profile + plugin install/state directories are created
    /// lazily (when a profile is provisioned, when a plugin is installed,
    /// when a plugin writes its first state key); only their parent roots
    /// are pre-created here so `read_dir` calls for `list_installed_plugins`
    /// don't have to special-case a missing tree on a fresh install.
    pub fn ensure_dirs(&self) -> AppResult<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(&self.avatars_dir)?;
        std::fs::create_dir_all(&self.metadata_artwork_dir)?;
        std::fs::create_dir_all(&self.profiles_dir)?;
        let plugin_paths = self.plugin_paths();
        std::fs::create_dir_all(&plugin_paths.plugins_root)?;
        Ok(())
    }

    /// Directory of a given profile (e.g. `<root>/profiles/42`).
    pub fn profile_dir(&self, profile_id: i64) -> PathBuf {
        self.profiles_dir.join(profile_id.to_string())
    }

    /// Per-profile database file (`<profile_dir>/data.db`).
    pub fn profile_db(&self, profile_id: i64) -> PathBuf {
        self.profile_dir(profile_id).join("data.db")
    }

    /// Per-profile artwork cache directory.
    pub fn profile_artwork_dir(&self, profile_id: i64) -> PathBuf {
        self.profile_dir(profile_id).join("artwork")
    }

    /// Create the directory layout required for a brand-new profile.
    pub fn ensure_profile_dirs(&self, profile_id: i64) -> AppResult<()> {
        std::fs::create_dir_all(self.profile_dir(profile_id))?;
        std::fs::create_dir_all(self.profile_artwork_dir(profile_id))?;
        Ok(())
    }

    /// Relative `data_dir` value stored in the `profile` table, so the layout
    /// stays portable if the app data root moves.
    pub fn profile_rel_dir(profile_id: i64) -> String {
        format!("profiles/{}", profile_id)
    }

    /// Plugin install + scratch roots, in [`PluginPaths`] form so the
    /// runtime can pass it straight into `PluginRuntime::load_plugin`
    /// and `new_store_for_plugin`. Layout:
    ///
    /// ```text
    /// <root>/plugins/<plugin-id>/       (install dir, read-only at runtime)
    /// <root>/plugin-data/<plugin-id>/   (per-user scratch, written by host imports)
    /// ```
    pub fn plugin_paths(&self) -> PluginPaths {
        PluginPaths::from_app_data(&self.root)
    }
}
