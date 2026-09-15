//! Where the rebuildable caches live (issue #619).
//!
//! Reported by email: WaveFlow stores artwork on the system drive even
//! when it is itself installed on another one, and a `C:` that is
//! critically low on space takes the whole machine down with it.
//!
//! # What moves, and why that is the line
//!
//! Only the **evictable** directories move. Every one of them is
//! content-addressed or LRU-evicted, so the worst outcome of a failed
//! move, a missing drive or a half-copy is a re-fetch. The databases,
//! the manually chosen motion covers and Canvas clips, and the offline
//! downloads all stay in the app-data tree: recreating those means the
//! user redoing work by hand, so moving them would be a migration
//! rather than a setting — with a restart, a schema guard to satisfy,
//! and a failure mode that loses data. That is the second option the
//! issue offers, and it is deliberately not the one taken here.
//!
//! # The three hazards, and where each is handled
//!
//! - **A move must never destroy the only copy.** [`set_cache_location`]
//!   copies, then persists the new location, and leaves the removal to
//!   the next startup. An interruption at any point leaves a whole copy
//!   on disk and a setting that names a whole copy.
//! - **The asset protocol has a static scope.** `tauri.conf.json` only
//!   allows `$APPDATA/…` and `$APPLOCALDATA/…`, so artwork served from
//!   another drive would silently fail to load — no error, no console
//!   message, just an image that never appears. [`grant_asset_scope`]
//!   widens it at runtime, which means every startup: a scope grant
//!   lives in the process, not on disk.
//! - **`AppPaths` is captured once, at boot.** `AppState` hands out a
//!   plain `AppPaths` and 95 call sites read it directly, so a move
//!   cannot take effect in the running process: every write after it
//!   would still land at the old root, and a background task holding a
//!   clone could split one batch of files across both. So the move
//!   copies and persists, and the *process restarts* to adopt the new
//!   location — after which a startup pass removes the old copy, at a
//!   moment when nothing is reading it. Deleting it eagerly, before the
//!   restart, would point the still-running app at directories that no
//!   longer exist.
//! - **A removable or network drive can be missing at launch.**
//!   [`resolve_cache_root`] falls back to the default location for that
//!   session *without clearing the stored choice*, so plugging the drive
//!   back in and relaunching picks it up again. The frontend is told,
//!   because a silent fallback looks exactly like the caches having been
//!   wiped.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};

use crate::{
    error::{AppError, AppResult},
    paths::AppPaths,
    state::AppState,
};

/// `app_setting` key holding the user's chosen cache root.
///
/// App-wide rather than per-profile: the shared caches have no profile,
/// and a per-profile answer would mean one profile's artwork on `D:` and
/// another's on `C:` for no benefit anyone asked for.
const KEY_CACHE_ROOT: &str = "storage.cache_root";

/// `app_setting` key holding the cache root a completed move left
/// behind, pending removal at the next startup.
///
/// It stores the old *cache root*, never a directory to delete outright:
/// moving away from the default would otherwise record the app-data root
/// itself, and removing that would take `app.db` and every profile with
/// it. The cleanup rebuilds the cache layout under the recorded root and
/// removes only those directories.
const KEY_CACHE_PENDING: &str = "storage.cache_root_pending_cleanup";

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
const OWNER_MARKER: &str = ".waveflow-cache";

/// Directory names the cache layout occupies directly under its root.
const OWNED_NAMES: &[&str] = &[
    "metadata_artwork",
    "motion_cache",
    "canvas_cache",
    "profiles",
];

/// Is this a cache root WaveFlow created?
fn is_owned(root: &Path) -> bool {
    root.join(OWNER_MARKER).is_file()
}

/// Claim a directory as ours, or explain why it cannot be.
///
/// Adoptable means: already marked, or holding none of the names the
/// layout would occupy. A folder with an unrelated `profiles/` in it is
/// refused — not because writing there would fail, but because a later
/// reset would delete it.
fn claim(root: &Path) -> Result<(), String> {
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

async fn load_path(app_db: &SqlitePool, key: &str) -> Option<PathBuf> {
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

async fn store_path(app_db: &SqlitePool, key: &str, root: Option<&Path>) -> AppResult<()> {
    store_path_in(app_db, key, root).await
}

/// The same write against any executor, so two of them can share one
/// transaction. See the pair in [`set_cache_location`].
async fn store_path_in<'e, E>(executor: E, key: &str, root: Option<&Path>) -> AppResult<()>
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
fn is_usable(root: &Path) -> Result<(), String> {
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

/// Remove the cache tree a completed move left behind.
///
/// Runs at startup, once the active root is known, which is the only
/// moment nothing is reading the old copy. Removes the cache
/// directories under the recorded root and nothing else — the recorded
/// root may well be the app-data root itself, which still holds
/// `app.db` and every profile.
///
/// The marker is kept when a removal was attempted and failed, so the
/// next startup tries again: a directory held open by an indexer or a
/// virus scanner is the normal case, it clears by itself, and giving up
/// after one attempt would strand a whole artwork tree for good. A root
/// we *refuse* to touch (no ownership marker, drive gone) clears it
/// instead -- retrying that one would never succeed.
///
/// Keeping it is only safe because `restart_required` no longer reads
/// this key on its own; see [`get_cache_location`].
pub async fn cleanup_moved_caches(active: &AppPaths, app_db: &SqlitePool) {
    let Some(previous) = load_path(app_db, KEY_CACHE_PENDING).await else {
        return;
    };
    // Kept when the cleanup cannot run — which is exactly the case a
    // session that fell back is in: `active.cache_root` is then the
    // default, `previous` is the default too, and clearing the marker
    // here would leave the copy that the move made behind for good.
    if previous == active.cache_root {
        return;
    }
    if owned_for_removal(&previous, active) {
        let stale_root = previous.clone();
        let stale = active.clone().with_cache_root(previous);
        let dirs = cache_dirs(&stale, app_db).await.unwrap_or_default();
        // On the blocking pool, like every other recursive delete here:
        // this one runs inside `AppState::init`, so walking a whole
        // artwork tree in place would hold the runtime through startup
        // -- the one moment the app has nothing else to show for it.
        let all_gone = tokio::task::spawn_blocking(move || {
            let mut all_gone = true;
            for (name, dir) in dirs {
                if !dir.exists() {
                    continue;
                }
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => {
                        tracing::info!(path = %dir.display(), %name, "removed moved-from cache")
                    }
                    Err(e) => {
                        all_gone = false;
                        tracing::warn!(
                            path = %dir.display(),
                            %name,
                            %e,
                            "could not remove a cache directory left behind by a move",
                        );
                    }
                }
            }
            all_gone
        })
        .await
        // A join failure is a panic in the loop above, which says
        // nothing about what is left on disk. Treated as unfinished.
        .unwrap_or(false);
        if !all_gone {
            tracing::warn!(
                "keeping the cache-cleanup marker; the next startup will try the rest again"
            );
            return;
        }
        // The tree is gone, so the claim on it goes too. Left behind,
        // `.waveflow-cache` keeps saying "WaveFlow owns this folder"
        // about a folder WaveFlow no longer uses -- which is the one
        // file that makes a recursive delete permissible here, sitting
        // in a directory the user picked and may well put something
        // else in. Never written at the app-data root, so never removed
        // from it either.
        if stale_root != active.root {
            match std::fs::remove_file(stale_root.join(OWNER_MARKER)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    path = %stale_root.display(),
                    %e,
                    "could not remove the ownership marker from the moved-from cache root",
                ),
            }
        }
    }
    if let Err(e) = store_path(app_db, KEY_CACHE_PENDING, None).await {
        tracing::warn!(%e, "could not clear the pending cache-cleanup marker");
    }
}

/// May a recursive delete run under this root?
///
/// Only when WaveFlow created the tree — or when the root *is* the
/// app-data root, which WaveFlow owns by construction and where no
/// marker is written. Every other answer is no, and the caller skips
/// the removal rather than taking a chance with a folder it did not
/// make.
fn owned_for_removal(root: &Path, paths: &AppPaths) -> bool {
    if root == paths.root {
        return true;
    }
    if is_owned(root) {
        return true;
    }
    tracing::warn!(
        path = %root.display(),
        "refusing to remove cache directories under a folder WaveFlow did not create",
    );
    false
}

/// Let the asset protocol read from the cache root.
///
/// Artwork reaches the frontend through `convertFileSrc`, which goes via
/// `asset://` and is gated by the scope declared in `tauri.conf.json` —
/// a static list of `$APPDATA` / `$APPLOCALDATA` patterns. A cache root
/// on another drive matches none of them, and the failure mode is an
/// image that never loads with nothing in the console, so this has to be
/// granted explicitly every time the process starts.
///
/// A no-op when the caches sit at the default location: the static scope
/// already covers that, and re-granting it costs a needless pattern.
pub fn grant_asset_scope(handle: &AppHandle, paths: &AppPaths) {
    if paths.cache_root == paths.root {
        return;
    }
    // The cache *subdirectories*, never the root itself. The root is a
    // folder the user picked in a file dialog, and it can perfectly
    // well be `D:\` or their home directory — granting the webview
    // recursive read access to all of it would hand every page in the
    // app a way to read the user's documents. Each of these is a
    // directory WaveFlow created and owns.
    let scope = handle.asset_protocol_scope();
    let profiles = paths.cache_root.join("profiles");
    let mut grants: Vec<&Path> = paths
        .shared_cache_dirs()
        .iter()
        .map(|(_, path)| path.as_path())
        .collect();
    grants.push(profiles.as_path());

    for dir in grants {
        if let Err(e) = scope.allow_directory(dir, true) {
            tracing::error!(
                path = %dir.display(),
                %e,
                "could not widen the asset scope to a cache directory; artwork stored there will not load",
            );
        }
    }
}

/// Recursive byte total, ignoring anything unreadable.
///
/// Iterative rather than recursive so a pathological tree cannot blow
/// the stack, and errors are skipped rather than propagated: this feeds
/// a number in a settings card, and one unreadable file should not turn
/// the whole reading into an error message.
fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(ft) if ft.is_file() => {
                    if let Ok(meta) = entry.metadata() {
                        total = total.saturating_add(meta.len());
                    }
                }
                // Symlinks are not followed: a link into the music
                // library would report the library's size as cache.
                _ => {}
            }
        }
    }
    total
}

/// Total bytes of a set of directories, off the async runtime.
///
/// A populated artwork tree is tens of thousands of small files, and
/// walking it in place would hold the runtime for the whole read —
/// which on the Settings page means every other command queued behind
/// a number nobody is waiting on.
async fn measure(dirs: Vec<(String, PathBuf)>) -> u64 {
    tokio::task::spawn_blocking(move || dirs.iter().map(|(_, path)| dir_size(path)).sum::<u64>())
        .await
        .unwrap_or(0)
}

/// Every cache directory that currently exists, app-wide and per
/// profile.
///
/// Built from [`AppPaths`] rather than from a second hand-written list,
/// so a directory added to the layout cannot be left out of the move or
/// the size reading.
/// The profile enumeration is **propagated**, not swallowed: a move
/// that silently saw no profiles would copy the shared caches, persist
/// the new location, and let the startup pass delete every profile's
/// artwork from the old one — none of it ever having been copied.
async fn cache_dirs(paths: &AppPaths, app_db: &SqlitePool) -> AppResult<Vec<(String, PathBuf)>> {
    let mut dirs: Vec<(String, PathBuf)> = paths
        .shared_cache_dirs()
        .iter()
        .map(|(name, path)| ((*name).to_string(), (*path).clone()))
        .collect();

    let profile_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM profile ORDER BY id")
        .fetch_all(app_db)
        .await?;

    for id in profile_ids {
        let rel = format!("profiles/{id}");
        dirs.push((format!("{rel}/artwork"), paths.profile_artwork_dir(id)));
        dirs.push((
            format!("{rel}/remote-artwork"),
            paths.profile_remote_artwork_dir(id),
        ));
        #[cfg(feature = "sync_v2")]
        dirs.push((
            format!("{rel}/remote-stream"),
            paths.profile_remote_stream_dir(id),
        ));
    }
    Ok(dirs)
}

/// Copy a directory tree. Returns the number of files written.
fn copy_tree(from: &Path, to: &Path) -> AppResult<u64> {
    if !from.exists() {
        return Ok(0);
    }
    std::fs::create_dir_all(to)?;
    let mut copied = 0u64;
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        for entry in std::fs::read_dir(&src)? {
            let entry = entry?;
            let target = dst.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                std::fs::create_dir_all(&target)?;
                stack.push((entry.path(), target));
            } else if file_type.is_file() {
                std::fs::copy(entry.path(), &target)?;
                copied += 1;
            }
            // Symlinks are skipped rather than followed or recreated: a
            // cache holds files we downloaded, so a link in there is
            // either someone's manual tinkering or a loop.
        }
    }
    Ok(copied)
}

/// Is `inner` the same as, or underneath, `outer`?
///
/// Used to refuse a move into the tree being moved, which would copy
/// files into their own destination for as long as the disk lasted. The
/// comparison is on the paths as given, canonicalised where possible —
/// `canonicalize` fails on a directory that does not exist yet, which is
/// the normal case for a freshly picked target, so a failure falls back
/// to the literal paths rather than refusing the move.
fn is_within(inner: &Path, outer: &Path) -> bool {
    let a = inner.canonicalize().unwrap_or_else(|_| inner.to_path_buf());
    let b = outer.canonicalize().unwrap_or_else(|_| outer.to_path_buf());
    a.starts_with(&b)
}

#[tauri::command]
pub async fn get_cache_location(state: tauri::State<'_, AppState>) -> AppResult<CacheLocation> {
    let paths = &state.paths;
    let configured = load_cache_root(&state.app_db).await;
    let dirs = cache_dirs(paths, &state.app_db).await?;
    let size_bytes = measure(dirs).await;

    // A stored choice that differs from the active root has two very
    // different causes, and telling the user the wrong one is worse
    // than saying nothing: either this session could not use it (the
    // drive is not there), or a move completed and is waiting for the
    // restart that adopts it.
    //
    // The pending-cleanup marker alone does not separate them, because
    // the two states overlap: a move to a drive that is unplugged
    // before the restart comes back to a session that fell back *and*
    // still carries the marker — [`cleanup_moved_caches`] deliberately
    // keeps it there, since the copy it names is the one being read.
    // Reading the marker on its own would then leave "restart WaveFlow"
    // on the card for good, and hide the fallback that is the thing
    // actually happening. `cache_root_fallback` is set by
    // [`resolve_cache_root`] in exactly the case that overlaps, so it
    // is the discriminator: a restart is pending only when this session
    // did not fall back.
    let move_pending = load_path(&state.app_db, KEY_CACHE_PENDING).await.is_some();
    // Only while the root that could not be used is still the chosen
    // one. Moving somewhere else answers the fallback, and a stale flag
    // would then both misreport it and swallow the restart notice.
    let fell_back = state
        .cache_root_fallback
        .as_ref()
        .is_some_and(|(root, _)| configured.as_ref() == Some(root));
    let diverged = configured
        .as_ref()
        .is_some_and(|chosen| chosen != &paths.cache_root);
    // Where the *next* session will read from, which is not the same
    // question as whether a choice is stored: going back to the default
    // clears the row rather than changing it, so `configured` is then
    // `None` and `diverged` is false while the running process is still
    // reading the old root. Testing divergence alone would drop the
    // restart notice for exactly the move most likely to be made twice.
    let next_root = configured.clone().unwrap_or_else(|| paths.root.clone());

    Ok(CacheLocation {
        active_root: paths.cache_root.to_string_lossy().to_string(),
        default_root: paths.root.to_string_lossy().to_string(),
        fell_back: diverged && fell_back,
        configured_root: configured.map(|p| p.to_string_lossy().to_string()),
        // Tied to the same condition as the flag above: a reason shown
        // beside a `fell_back` of `false` is a sentence about a drive
        // the card is no longer describing.
        fallback_reason: (diverged && fell_back)
            .then(|| {
                state
                    .cache_root_fallback
                    .as_ref()
                    .map(|(_, why)| why.clone())
            })
            .flatten(),
        // Compared against where the next session will read from, not
        // against the marker alone -- that is what lets
        // `cleanup_moved_caches` keep the marker across a failed
        // removal: once the process has adopted the new root, the two
        // agree and no restart is outstanding, whatever is still
        // sitting in the old tree.
        restart_required: move_pending && !fell_back && next_root != paths.cache_root,
        size_bytes,
    })
}

/// Point the caches at `root` — or back at the default when it is
/// `None` — copying what is already there.
///
/// Copy, persist, restart, and only then delete. Power loss between any
/// two of those steps leaves a complete copy on disk and a setting that
/// names a complete copy; nothing is removed until a fresh process has
/// adopted the new location.
///
/// Returns with `restart_required` set rather than restarting here, so
/// the caller can say what is about to happen — a move can take a while
/// on a large library, and a window that vanishes without warning at the
/// end of it reads as a crash.
#[tauri::command]
pub async fn set_cache_location(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    root: Option<String>,
) -> AppResult<CacheLocation> {
    // Held from before the pending-move check to past the last
    // persistence write. Everything between them is a check followed by
    // a side effect, across awaits — see `AppState::cache_move_lock`.
    let _serialized = state.cache_move_lock.clone().lock_owned().await;

    let paths = state.paths.clone();
    let target = match root.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => paths.root.clone(),
    };

    // Compared against the *stored* choice, not only the active root. A
    // session that fell back is already running on the default, so
    // "back to default" would look like a no-op and leave the setting
    // pointing at the drive that is not there — which is the one moment
    // the user is most likely to press it.
    let stored_now = load_cache_root(&state.app_db).await;
    let already_there = target == paths.cache_root
        && stored_now.as_deref() == (target != paths.root).then_some(target.as_path());
    if already_there {
        return get_cache_location(state).await;
    }

    // Asking for the root the caches are *already* at. Two different
    // people arrive here, and neither can be served by the copy below:
    //
    // - somebody cancelling a move they staged a minute ago and have
    //   not restarted for;
    // - somebody whose chosen drive was missing at launch, so this
    //   session fell back to the default and they are now asking to
    //   stay there.
    //
    // Both were previously refused outright -- by the staged-move guard
    // below, and then again by the containment check, which is right to
    // call a tree "inside the folder being moved" when it *is* that
    // folder. The second case is a dead end with no way out of it:
    // restarting cannot finish a move whose destination is unplugged,
    // and no other destination could be chosen either.
    //
    // Nothing is copied, because source and destination are one tree
    // and `copy_tree` would walk it copying every file onto itself. All
    // that changes is the stored choice -- and the cleanup marker,
    // which is retargeted at the abandoned destination so the copy
    // staged there is removed at the next startup instead of being
    // orphaned.
    if target == paths.cache_root {
        let abandoned = load_cache_root(&state.app_db)
            .await
            .filter(|configured| configured != &paths.cache_root);
        let stored = (target != paths.root).then(|| target.clone());
        // One transaction, like the staging path below. Written apart,
        // a crash between them leaves the choice cancelled while the
        // marker still names a destination -- and the "one staged move
        // at a time" guard would then refuse every later move, with no
        // way to clear what it is reading.
        let mut tx = state.app_db.begin().await?;
        store_path_in(&mut *tx, KEY_CACHE_ROOT, stored.as_deref()).await?;
        store_path_in(&mut *tx, KEY_CACHE_PENDING, abandoned.as_deref()).await?;
        tx.commit().await?;
        return get_cache_location(state).await;
    }
    // Created *before* the containment check, not after: `canonicalize`
    // fails on a directory that does not exist, and the fallback to the
    // literal paths cannot see through a symlink, a junction, or a
    // difference in case on a case-insensitive filesystem — all of
    // which would let the copy run into its own source.
    // One staged move at a time. A second one before the restart would
    // overwrite the record of what to clean up, and the first
    // destination's copy would be orphaned on disk with nothing left
    // pointing at it — the caches are rebuildable, but leaving a
    // duplicate of a whole artwork tree behind is the opposite of what
    // the person moving them asked for.
    if load_path(&state.app_db, KEY_CACHE_PENDING).await.is_some() {
        return Err(AppError::Other(
            "a cache move is already staged; restart WaveFlow to finish it".into(),
        ));
    }

    // Whether the folder was already there, read before `is_usable`
    // creates it: a move refused below must not leave a directory
    // behind, and must not remove one the user already had.
    let target_existed = target.exists();
    is_usable(&target).map_err(AppError::Other)?;
    // Refused *before* the claim below, not after. `claim` writes the
    // ownership marker, and the marker is what makes a folder
    // deletable: leaving one behind in a directory whose move was
    // rejected would hand a later reset permission to wipe a folder
    // WaveFlow never adopted. The check needs `is_usable` to have run
    // first, since `canonicalize` cannot see through a junction or a
    // case difference on a directory that does not exist yet -- which
    // is exactly how a copy ends up running into its own source.
    if is_within(&target, &paths.cache_root) {
        // `remove_dir`, never `remove_dir_all`, and only for a
        // directory this call brought into being: it refuses a
        // non-empty one, so the worst case of getting the ownership
        // question wrong is an empty folder left behind rather than
        // somebody's files gone.
        if !target_existed {
            let _ = std::fs::remove_dir(&target);
        }
        return Err(AppError::Other(format!(
            "{} is inside the folder being moved",
            target.display()
        )));
    }
    // Claimed before anything is copied into it: a folder that already
    // holds one of the layout's names and was not made by us is refused
    // outright, because adopting it would put a later reset in a
    // position to delete what is there.
    //
    // Except the app-data root, which WaveFlow owns by construction and
    // where no marker is written — the same exception `owned_for_removal`
    // makes. Without it, "back to default" is *refused*: that root does
    // hold a `profiles/` directory, and it has no marker to explain
    // itself.
    // Whether the marker was already there, so a failure below removes
    // only what this attempt wrote. Read before `claim`, which is what
    // writes it.
    let marker_existed = is_owned(&target);
    if target != paths.root {
        claim(&target).map_err(AppError::Other)?;
    }

    let moved = paths.clone().with_cache_root(target.clone());
    let sources = cache_dirs(&paths, &state.app_db).await?;
    // Derived from the sources by their relative name, not read a
    // second time: a profile created or deleted between two calls would
    // shift one list against the other, and the `zip` below would then
    // copy one profile's artwork into another's directory.
    let destinations: Vec<(String, PathBuf)> = sources
        .iter()
        .map(|(name, _)| (name.clone(), target.join(name)))
        .collect();

    // Copy on the blocking pool: a library's worth of artwork is
    // thousands of small files, which would stall the runtime.
    let plan: Vec<(String, PathBuf, PathBuf)> = sources
        .iter()
        .zip(destinations.iter())
        .map(|((name, from), (_, to))| (name.clone(), from.clone(), to.clone()))
        .collect();
    // Which destinations were already on disk, for the same reason as
    // the marker: a folder the user had before this attempt is not ours
    // to remove when it fails.
    let pre_existing: Vec<bool> = destinations.iter().map(|(_, to)| to.exists()).collect();

    let copy = tokio::task::spawn_blocking(move || -> AppResult<()> {
        for (name, from, to) in plan {
            copy_tree(&from, &to).map_err(|e| {
                AppError::Other(format!("copying {name} to {} failed: {e}", to.display()))
            })?;
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(format!("cache copy task failed: {e}")));

    // A copy that stopped part-way has adopted a folder and filled some
    // of it, and nothing records either: the setting is written further
    // down and only on success, so without this the user is left with a
    // half-copied artwork tree and a marker saying WaveFlow owns the
    // folder -- which is the one file that lets a later reset delete it.
    // Rolled back to exactly what was there before, never further.
    if let Err(err) = copy.and_then(|inner| inner) {
        for ((_, dir), existed) in destinations.iter().zip(pre_existing.iter()) {
            if !existed {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        if !marker_existed && target != paths.root {
            let _ = std::fs::remove_file(target.join(OWNER_MARKER));
        }
        return Err(err);
    }

    // Both writes or neither. They used to go one after the other, on
    // the reasoning that landing only the first comes back reading a
    // whole copy -- true, but it also comes back with no record of the
    // tree left behind, and since the cleanup marker is now one of the
    // roots `candidate_cache_roots` reports, losing it means a full
    // artwork tree that no sweep in the app can name.
    let stored = (target != paths.root).then(|| target.clone());
    let mut tx = state.app_db.begin().await?;
    store_path_in(&mut *tx, KEY_CACHE_ROOT, stored.as_deref()).await?;
    store_path_in(&mut *tx, KEY_CACHE_PENDING, Some(&paths.cache_root)).await?;
    tx.commit().await?;
    grant_asset_scope(&app, &moved);

    // `active_root` is still the old one, and saying otherwise would be
    // a lie with consequences: the process keeps reading and writing
    // there until it restarts, so a card showing the new path while the
    // next cover lands at the old one is exactly the confusion the
    // restart notice exists to prevent. The destination is reported as
    // `configured_root`, which is what it is.
    let size_bytes = measure(destinations).await;
    Ok(CacheLocation {
        active_root: paths.cache_root.to_string_lossy().to_string(),
        default_root: moved.root.to_string_lossy().to_string(),
        configured_root: stored.map(|p| p.to_string_lossy().to_string()),
        fell_back: false,
        fallback_reason: None,
        restart_required: true,
        size_bytes,
    })
}

/// Restart so the new cache location takes effect.
///
/// Separate from [`set_cache_location`] because that one has to be able
/// to return: the frontend tells the user a restart is coming, and the
/// restart happens when they say so. Diverges — the process is replaced.
#[tauri::command]
pub async fn restart_for_cache_move(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<()> {
    // Behind the same lock the move itself holds. Replacing the process
    // mid-copy would leave a half-copied tree with neither setting
    // written, and the command is reachable from the IPC surface
    // whatever the button is doing. Waiting is the whole of it: the
    // move either has not started, or this returns once it is durable.
    let _serialized = state.cache_move_lock.clone().lock_owned().await;
    app.restart();
}

/// Every root a cache tree of ours can be sitting under right now.
///
/// Three, and each is reachable on its own:
///
/// - the **active** root, which this session reads and writes;
/// - the **configured** one, which differs while a move waits for its
///   restart and again whenever a session falls back;
/// - the root a completed move left behind. Normally gone by the time
///   anything asks — [`cleanup_moved_caches`] empties it at the next
///   startup — but it survives a removal that failed, and then neither
///   of the other two names it. A tree nothing can name is a tree
///   nothing will ever remove.
///
/// Deduplicated, in that order. The callers filter and gate it: none of
/// them may touch a root without an ownership marker, and none of them
/// wants the app-data root, which their own wholesale delete covers.
async fn candidate_cache_roots(state: &AppState) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![state.paths.cache_root.clone()];
    for extra in [
        load_cache_root(&state.app_db).await,
        load_path(&state.app_db, KEY_CACHE_PENDING).await,
    ]
    .into_iter()
    .flatten()
    {
        if !roots.contains(&extra) {
            roots.push(extra);
        }
    }
    roots
}

/// Every place a deleted profile's caches can still be sitting.
///
/// `profile_dir` covers the app-data tree, and that used to be the
/// whole answer. Since #619 there are two more roots to look under, and
/// they can both hold a full copy at the same time:
///
/// - the **active** cache root, which is where this session has been
///   reading and writing;
/// - the **configured** one, when it differs — which happens while a
///   move is staged and waiting for the restart that adopts it, and
///   again whenever a session falls back because the drive is not
///   plugged in.
///
/// Missing the second is not a small leak: nothing enumerates a deleted
/// profile afterwards — [`cache_dirs`] builds its list from the
/// `profile` table — so a whole artwork tree would stay there with
/// nothing left that could ever name it.
///
/// Each root is gated by the same ownership check every other recursive
/// delete in this module uses, because one of them is a folder the user
/// picked in a dialog.
pub async fn profile_cache_dirs_elsewhere(state: &AppState, profile_id: i64) -> Vec<PathBuf> {
    let roots = candidate_cache_roots(state).await;
    roots
        .into_iter()
        // The app-data root is what `profile_dir` already removes;
        // naming it again would mean deleting the same tree twice.
        .filter(|root| *root != state.paths.root)
        .filter(|root| owned_for_removal(root, &state.paths))
        .map(|root| state.paths.clone().with_cache_root(root).profile_cache_dir(profile_id))
        .collect()
}

/// Cache directories that a full reset would otherwise miss.
///
/// `reset_app` removes [`AppPaths::root`], which used to be the whole
/// story. Since the caches can be moved off that tree (#619), a reset
/// that only wipes `root` leaves gigabytes of artwork on whichever drive
/// the user moved them to — which is precisely the disk usage they moved
/// them there to control.
///
/// Returns the moved-to directories only: when the caches sit at the
/// default location, `remove_dir_all(root)` already covers them and
/// naming them again would mean deleting the same tree twice.
pub async fn wipe_targets_outside_root(state: &AppState) -> Vec<PathBuf> {
    // Both roots, for the same reason `profile_cache_dirs_elsewhere`
    // takes both: while a move waits for its restart -- and again
    // whenever a session falls back -- a full copy exists under the
    // active root *and* under the configured one. Keyed only on the
    // active root, a reset staged right after a move would wipe the old
    // tree and leave the new one untouched, which is the copy the user
    // is about to start reading.
    let roots = candidate_cache_roots(state).await;

    let mut targets = Vec::new();
    for root in roots {
        // The app-data root is what `reset_app` removes wholesale;
        // naming it here would mean deleting the same tree twice.
        if root == state.paths.root {
            continue;
        }
        // The same guard the deferred cleanup uses. A reset is the most
        // destructive path in the app, and the cache root is a folder
        // the user picked — if WaveFlow did not create the tree, it does
        // not get to remove it.
        if !owned_for_removal(&root, &state.paths) {
            continue;
        }
        // A failure to enumerate profiles here means the reset removes
        // only what it could name. Logged rather than propagated: the
        // wipe is already under way and stopping it halfway is worse
        // than leaving a cache behind.
        let dirs = cache_dirs(&state.paths.clone().with_cache_root(root), &state.app_db)
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "could not enumerate relocated cache directories for the reset");
                Vec::new()
            });
        targets.extend(dirs.into_iter().map(|(_, path)| path));
    }
    targets
}
