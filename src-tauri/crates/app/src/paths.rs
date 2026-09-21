use std::path::PathBuf;

use crate::error::{AppError, AppResult};

/// Resolved filesystem paths for the application.
///
/// Layout (on Windows example, equivalent on macOS/Linux via Tauri's data dir):
///
/// ```text
/// <app_data>/waveflow/                 <- `root`
/// ├── app.db                    (global registry + app settings)
/// ├── avatars/                  (shared profile avatars, hash-addressed)
/// └── profiles/
///     └── <profile_id>/
///         ├── data.db           (per-profile database)
///         ├── motion/           (per-profile manual motion covers, never evicted)
///         ├── canvas/           (per-profile manual Canvas clips, never evicted)
///         └── remote-downloads/ (offline copies the user asked for)
///
/// <cache_root>/                        <- `root` unless the user moved it
/// ├── metadata_artwork/         (shared remote artwork cache, hash-addressed)
/// ├── motion_cache/             (shared animated-cover LRU)
/// ├── canvas_cache/             (shared per-track Canvas LRU)
/// └── profiles/
///     └── <profile_id>/
///         ├── artwork/          (per-profile artwork cache)
///         ├── remote-artwork/   (per-profile remote cover cache)
///         └── remote-stream/    (per-profile remote audio cache)
/// ```
///
/// # Why two roots (issue #619)
///
/// Artwork landed on the system drive whatever drive WaveFlow itself was
/// installed on, and a `C:` that is critically low on space takes the
/// whole machine down with it. So the growing, *rebuildable* part of the
/// tree can be pointed somewhere else.
///
/// The split is not "big things move". It is **evictable moves, chosen
/// stays**: every directory under `cache_root` is content-addressed or
/// LRU-evicted, so the worst case of a failed or missing move is a
/// re-fetch. The databases, the manual motion covers, the manual Canvas
/// clips and the offline downloads are things the user would have to
/// recreate by hand, so moving them would be a migration rather than a
/// setting, and they stay put.
///
/// `bundled_plugins_dir` is resolved separately against
/// [`BaseDirectory::Resource`] and points at the installer-shipped
/// plugin tree (e.g. `<install>/plugins/` on Windows NSIS,
/// `/usr/lib/WaveFlow/plugins/` on Linux). It's optional because the
/// resource resolver can legitimately fail in a few environments
/// (dev mode without a bundle, packaging mishaps); when missing,
/// bundled plugin resolution falls back to the writable app-data
/// tree — the cleanup pass at boot removes any stale copies there
/// so the fallback only matters for tests + dev builds.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub root: PathBuf,
    /// Where the rebuildable caches live. Equal to [`Self::root`] unless
    /// the user moved them (issue #619).
    pub cache_root: PathBuf,
    pub app_db: PathBuf,
    pub avatars_dir: PathBuf,
    pub metadata_artwork_dir: PathBuf,
    /// App-wide, opt-in LRU cache of downloaded animated-album-artwork
    /// (`.mp4`) files — shared across profiles like `metadata_artwork`.
    pub motion_cache_dir: PathBuf,
    /// App-wide, opt-in LRU cache of downloaded per-track Canvas (`.mp4`)
    /// files (issue #473) — same shape as `motion_cache_dir`, separate dir
    /// so the two caches size/evict/clear independently.
    pub canvas_cache_dir: PathBuf,
    pub profiles_dir: PathBuf,
    pub bundled_plugins_dir: Option<PathBuf>,
}

impl AppPaths {
    /// Same layout, resolved from the app-data root alone.
    ///
    /// The Tauri host adapter adds the `BaseDirectory::Resource` lookup,
    /// which needs a live app. The startup pre-flight
    /// ([`crate::db::schema_guard::preflight`]) runs *before* the Tauri
    /// event loop exists — there is no `AppHandle` to resolve against
    /// yet — and it only ever reads databases, so it takes this
    /// constructor and leaves `bundled_plugins_dir` at `None`.
    ///
    /// Creates nothing on disk, like the Tauri host adapter.
    pub fn from_root(root: PathBuf, bundled_plugins_dir: Option<PathBuf>) -> Self {
        Self {
            app_db: root.join("app.db"),
            avatars_dir: root.join("avatars"),
            metadata_artwork_dir: root.join("metadata_artwork"),
            motion_cache_dir: root.join("motion_cache"),
            canvas_cache_dir: root.join("canvas_cache"),
            profiles_dir: root.join("profiles"),
            bundled_plugins_dir,
            cache_root: root.clone(),
            root,
        }
    }

    /// Same layout with the caches rooted somewhere else (issue #619).
    ///
    /// Only the evictable directories move — see the type-level docs for
    /// why. Passing the default root back in is a no-op, which is what
    /// the "put it back" path in Settings does.
    #[must_use]
    pub fn with_cache_root(mut self, cache_root: PathBuf) -> Self {
        self.metadata_artwork_dir = cache_root.join("metadata_artwork");
        self.motion_cache_dir = cache_root.join("motion_cache");
        self.canvas_cache_dir = cache_root.join("canvas_cache");
        self.cache_root = cache_root;
        self
    }

    /// Every directory that moves with [`Self::cache_root`], paired with
    /// its path relative to that root.
    ///
    /// The app-wide ones only. Per-profile caches are enumerated by
    /// [`Self::profile_cache_dir`], because there is no list of profile
    /// ids at this level.
    ///
    /// Used by the move: copying and verifying walk the same list the
    /// layout is built from, so a directory added to one is impossible
    /// to forget in the other.
    pub fn shared_cache_dirs(&self) -> [(&'static str, &PathBuf); 3] {
        [
            ("metadata_artwork", &self.metadata_artwork_dir),
            ("motion_cache", &self.motion_cache_dir),
            ("canvas_cache", &self.canvas_cache_dir),
        ]
    }

    /// The app-data root for a bundle identifier, without a running app.
    ///
    /// Mirrors what Tauri's own resolver does — `app_data_dir()` is
    /// `dirs::data_dir()/<identifier>` (see `tauri::path::PathResolver`)
    /// — plus the `waveflow/` subdirectory the Tauri host adapter appends.
    /// The identifier is read from the generated context at the call
    /// site rather than hardcoded, so `tauri.conf.json` stays the single
    /// source of truth.
    pub fn root_for_identifier(identifier: &str) -> AppResult<PathBuf> {
        dirs::data_dir()
            .ok_or(AppError::MissingAppDataDir)
            .map(|dir| dir.join(identifier).join("waveflow"))
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
        std::fs::create_dir_all(&self.cache_root)?;
        std::fs::create_dir_all(&self.avatars_dir)?;
        std::fs::create_dir_all(&self.metadata_artwork_dir)?;
        std::fs::create_dir_all(&self.motion_cache_dir)?;
        std::fs::create_dir_all(&self.canvas_cache_dir)?;
        std::fs::create_dir_all(&self.profiles_dir)?;
        std::fs::create_dir_all(self.root.join("plugins"))?;
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

    /// Directory holding a profile's *caches* (e.g.
    /// `<cache_root>/profiles/42`).
    ///
    /// Identical to [`Self::profile_dir`] until the user moves the
    /// caches; the two diverge from that point on, which is why every
    /// evictable per-profile directory below is built from this one and
    /// every kept one from `profile_dir`.
    pub fn profile_cache_dir(&self, profile_id: i64) -> PathBuf {
        self.cache_root
            .join("profiles")
            .join(profile_id.to_string())
    }

    /// Per-profile artwork cache directory.
    pub fn profile_artwork_dir(&self, profile_id: i64) -> PathBuf {
        self.profile_cache_dir(profile_id).join("artwork")
    }

    /// Per-profile directory for user-supplied animated album covers
    /// (issue #408). Unlike [`Self::motion_cache_dir`] — a 1 GB LRU that
    /// evicts plugin-downloaded mp4s by mtime — a file here was chosen
    /// deliberately by the user and must never be evicted.
    pub fn profile_motion_dir(&self, profile_id: i64) -> PathBuf {
        self.profile_dir(profile_id).join("motion")
    }

    /// Per-profile directory for user-supplied per-track Canvas clips
    /// (issue #442). Like [`Self::profile_motion_dir`] and unlike
    /// [`Self::motion_cache_dir`], a file here was chosen deliberately by the
    /// user and is never evicted; kept separate from `motion/` so a track's
    /// 15 s Canvas and an album's motion cover don't share a namespace.
    pub fn profile_canvas_dir(&self, profile_id: i64) -> PathBuf {
        self.profile_dir(profile_id).join("canvas")
    }

    /// Per-profile cache for the remote server's cover art (RFC-005).
    /// Unlike [`Self::profile_motion_dir`] and [`Self::profile_canvas_dir`],
    /// nothing here was chosen by the user: every file is a download that is
    /// content-addressed and reproducible, so this one *is* evictable — on
    /// the same terms as [`Self::motion_cache_dir`].
    pub fn profile_remote_artwork_dir(&self, profile_id: i64) -> PathBuf {
        self.profile_cache_dir(profile_id).join("remote-artwork")
    }

    /// Downloaded copies of the bound server's tracks — the managed folder.
    ///
    /// Deliberately not a scanned library folder: a download is an offline
    /// copy of a *remote* track, not a new local one, and letting the scanner
    /// index it would create a second entry for a song the library already
    /// knows through the server.
    ///
    /// Separate from the stream cache next door because the two have opposite
    /// lifetimes: the cache is evicted under a budget without asking, and a
    /// download disappears only when its owner says so.
    /// Only the remote source uses this, so it does not exist without it.
    #[cfg(feature = "sync_v2")]
    pub fn profile_remote_download_dir(&self, profile_id: i64) -> PathBuf {
        self.profile_dir(profile_id).join("remote-downloads")
    }

    /// Cached audio for the bound server's tracks. Beside the cover cache
    /// rather than inside it: these are whole songs, and the settings card
    /// reports and clears the two separately because their sizes are orders
    /// of magnitude apart.
    /// Only the remote source uses this, so it does not exist without it.
    #[cfg(feature = "sync_v2")]
    pub fn profile_remote_stream_dir(&self, profile_id: i64) -> PathBuf {
        self.profile_cache_dir(profile_id).join("remote-stream")
    }

    /// Create the directory layout required for a brand-new profile.
    pub fn ensure_profile_dirs(&self, profile_id: i64) -> AppResult<()> {
        std::fs::create_dir_all(self.profile_dir(profile_id))?;
        std::fs::create_dir_all(self.profile_cache_dir(profile_id))?;
        std::fs::create_dir_all(self.profile_artwork_dir(profile_id))?;
        std::fs::create_dir_all(self.profile_motion_dir(profile_id))?;
        std::fs::create_dir_all(self.profile_canvas_dir(profile_id))?;
        // Profiles that predate this directory get it on first write instead;
        // the cache creates it on demand rather than trusting this to have run.
        std::fs::create_dir_all(self.profile_remote_artwork_dir(profile_id))?;
        Ok(())
    }

    /// Relative `data_dir` value stored in the `profile` table, so the layout
    /// stays portable if the app data root moves.
    pub fn profile_rel_dir(profile_id: i64) -> String {
        format!("profiles/{}", profile_id)
    }
}
