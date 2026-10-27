//! Removing the caches a move left behind, and the asset scope granted to
//! the root in use.

use std::path::Path;

use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};

use crate::paths::AppPaths;

use super::*;

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
pub(super) fn owned_for_removal(root: &Path, paths: &AppPaths) -> bool {
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
