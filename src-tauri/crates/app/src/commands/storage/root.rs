//! The cache root: its settings, the marker that claims a folder, whether
//! a folder is usable, and the root resolved at startup.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{error::AppResult, paths::AppPaths};

/// `app_setting` key holding the user's chosen cache root.
///
/// App-wide rather than per-profile: the shared caches have no profile,
/// and a per-profile answer would mean one profile's artwork on `D:` and
/// another's on `C:` for no benefit anyone asked for.
pub(super) const KEY_CACHE_ROOT: &str = "storage.cache_root";

/// `app_setting` key holding the cache root a completed move left
/// behind, pending removal at the next startup.
///
/// It stores the old *cache root*, never a directory to delete outright:
/// moving away from the default would otherwise record the app-data root
/// itself, and removing that would take `app.db` and every profile with
/// it. The cleanup rebuilds the cache layout under the recorded root and
/// removes only those directories.
pub(super) const KEY_CACHE_PENDING: &str = "storage.cache_root_pending_cleanup";

/// Prefix of the probe file [`is_usable`] creates and removes.
///
/// A directory can exist, be listable, and still refuse writes — a
/// read-only network share, a drive mounted by another user, a folder
/// inside a container the app has no grant for. Only a write answers the
/// question the caller is actually asking.
///
/// The name is made unique per call and the file is created with
/// `create_new`, so the probe can never truncate or delete something
/// the user already had there. The directory being probed is one they
/// just picked in a file dialog; writing into it is a liberty, and
/// destroying a file in it is not one to take on a name collision.
const PROBE_PREFIX: &str = ".waveflow-write-probe";

/// File written at the root of a cache location WaveFlow manages.
///
/// The guard on every recursive delete in this module, and the reason
/// it exists: the cache layout is a set of ordinary directory names —
/// `metadata_artwork`, `motion_cache`, `profiles` — derived from a root
/// the user picked in a file dialog. Pick a folder that already has a
/// `profiles/` directory in it and, without this marker, a reset would
/// remove it. The marker says "WaveFlow made this tree"; nothing
/// without it is ever deleted, and a root that already holds one of our
/// names but no marker is refused rather than adopted.
pub(super) const OWNER_MARKER: &str = ".waveflow-cache";

/// Directory names the cache layout occupies directly under its root.
pub(super) const OWNED_NAMES: &[&str] = &[
    "metadata_artwork",
    "motion_cache",
    "canvas_cache",
    "profiles",
];

/// Is this a cache root WaveFlow created?
pub(super) fn is_owned(root: &Path) -> bool {
    root.join(OWNER_MARKER).is_file()
}

/// Claim a directory as ours, or explain why it cannot be.
///
/// Adoptable means: already marked, or holding none of the names the
/// layout would occupy. A folder with an unrelated `profiles/` in it is
/// refused — not because writing there would fail, but because a later
/// reset would delete it.
pub(super) fn claim(root: &Path) -> Result<(), String> {
    if is_owned(root) {
        return Ok(());
    }
    // Propagated, not swallowed. This loop is the whole of the guard:
    // it answers "is there something here we would later delete?", and
    // a directory we could not enumerate has not answered it. Writing
    // the marker anyway would adopt -- and make deletable -- a folder
    // whose contents we never managed to look at.
    let entries =
        std::fs::read_dir(root).map_err(|e| format!("cannot inspect {}: {e}", root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot inspect {}: {e}", root.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if OWNED_NAMES.iter().any(|owned| name.as_ref() == *owned) {
            return Err(format!(
                "{} already contains a \"{name}\" folder that WaveFlow did not create",
                root.display()
            ));
        }
    }
    std::fs::write(
        root.join(OWNER_MARKER),
        b"WaveFlow cache directory. Removing this file makes WaveFlow \
refuse to manage or clean this folder.\n",
    )
    .map_err(|e| format!("cannot mark {} as a cache folder: {e}", root.display()))
}

/// What the Settings card needs to render the current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheLocation {
    /// Where the caches are being read and written right now.
    pub active_root: String,
    /// Where they live when nothing has been chosen.
    pub default_root: String,
    /// The stored choice, when there is one. Present *and* different from
    /// `active_root` means this session fell back.
    pub configured_root: Option<String>,
    /// True when a stored choice could not be used this session.
    pub fell_back: bool,
    /// Why, when it did. Shown verbatim: "drive not found" and
    /// "permission denied" call for different actions from the user.
    pub fallback_reason: Option<String>,
    /// True once a move has been staged and the process has to restart
    /// before it takes effect.
    pub restart_required: bool,
    /// Total bytes currently under `active_root`, for the four cache
    /// families together.
    pub size_bytes: u64,
}

pub(super) async fn load_path(app_db: &SqlitePool, key: &str) -> Option<PathBuf> {
    sqlx::query_scalar::<_, String>("SELECT value FROM app_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(app_db)
        .await
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

/// Read the stored choice. `None` means "wherever the default is".
pub async fn load_cache_root(app_db: &SqlitePool) -> Option<PathBuf> {
    load_path(app_db, KEY_CACHE_ROOT).await
}

pub(super) async fn store_path(
    app_db: &SqlitePool,
    key: &str,
    root: Option<&Path>,
) -> AppResult<()> {
    store_path_in(app_db, key, root).await
}

/// The same write against any executor, so two of them can share one
/// transaction. See the pair in [`set_cache_location`].
pub(super) async fn store_path_in<'e, E>(
    executor: E,
    key: &str,
    root: Option<&Path>,
) -> AppResult<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    match root {
        Some(path) => {
            sqlx::query(
                "INSERT INTO app_setting (key, value, value_type, updated_at)
                 VALUES (?, ?, 'string', ?)
                 ON CONFLICT(key) DO UPDATE
                   SET value = excluded.value,
                       value_type = excluded.value_type,
                       updated_at = excluded.updated_at",
            )
            .bind(key)
            .bind(path.to_string_lossy().to_string())
            .bind(Utc::now().timestamp())
            .execute(executor)
            .await?;
        }
        // Deleting rather than storing an empty string: "no row" is
        // already the shape `load_cache_root` treats as the default, and
        // two spellings of the same state is how a reset ends up half
        // applied.
        None => {
            sqlx::query("DELETE FROM app_setting WHERE key = ?")
                .bind(key)
                .execute(executor)
                .await?;
        }
    }
    Ok(())
}

/// Can we actually put cache files here?
///
/// Creates the directory if it is missing, then writes and removes a
/// probe file. Returns the reason on failure so the caller can log or
/// show something better than "it did not work".
pub(super) fn is_usable(root: &Path) -> Result<(), String> {
    use std::io::Write;

    std::fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe = root.join(format!("{PROBE_PREFIX}-{unique}"));

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|e| format!("cannot write to {}: {e}", root.display()))?;
    let written = file
        .write_all(b"waveflow")
        .map_err(|e| format!("cannot write to {}: {e}", root.display()));
    drop(file);
    // Removed whatever the write did, since this call is what created
    // it. A failure to clean up is not a failure to be usable.
    let _ = std::fs::remove_file(&probe);
    written
}

/// Decide where the caches live for this session.
///
/// Returns the paths to use and, when a stored choice had to be
/// ignored, the reason — which the caller surfaces rather than
/// swallowing: caches silently reappearing at the default location is
/// indistinguishable, from the user's side, from them having been
/// deleted.
pub async fn resolve_cache_root(
    paths: AppPaths,
    app_db: &SqlitePool,
) -> (AppPaths, Option<(PathBuf, String)>) {
    let Some(configured) = load_cache_root(app_db).await else {
        return (paths, None);
    };
    if configured == paths.root {
        return (paths, None);
    }
    match is_usable(&configured).and_then(|()| {
        if configured == paths.root {
            Ok(())
        } else {
            claim(&configured)
        }
    }) {
        Ok(()) => (paths.with_cache_root(configured), None),
        Err(reason) => {
            tracing::warn!(
                path = %configured.display(),
                %reason,
                "configured cache root unusable; falling back to the app-data tree for this session",
            );
            // The stored choice is deliberately left alone: the drive may
            // simply not be plugged in, and clearing it would turn a
            // temporary absence into a permanent reset.
            //
            // Returned with the root it refers to: see
            // `AppState::cache_root_fallback`.
            (paths, Some((configured, reason)))
        }
    }
}
