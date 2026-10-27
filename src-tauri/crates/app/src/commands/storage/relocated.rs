//! The caches a profile has outside the active root, found so a wipe
//! reaches them too.

use std::path::PathBuf;

use crate::state::AppState;

use super::*;

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

/// One relocated cache root a reset has to clear.
pub(crate) struct RelocatedCache {
    /// Removed once the directories are gone -- never before. A marker
    /// deleted while a directory it vouches for survives would leave
    /// that tree with nothing left in the app entitled to remove it.
    pub marker: PathBuf,
    pub dirs: Vec<PathBuf>,
}

/// What a full reset would otherwise miss, one relocated root at a time.
///
/// `reset_app` removes [`AppPaths::root`], which used to be the whole
/// story. Since the caches can be moved off that tree (#619), a reset
/// that only wipes `root` leaves gigabytes of artwork on whichever drive
/// the user moved them to — which is precisely the disk usage they moved
/// them there to control.
///
/// Returns moved-to roots only: when the caches sit at the default
/// location, `remove_dir_all(root)` already covers them and naming them
/// again would mean deleting the same tree twice.
pub async fn wipe_targets_outside_root(state: &AppState) -> Vec<RelocatedCache> {
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
        // Skipped whole, not reported with an empty list. An empty
        // list means "this root holds nothing", and the caller reads
        // that as every directory removed and takes the marker away --
        // which would strand the caches actually sitting there with
        // nothing in the app entitled to remove them. Keeping the
        // marker leaves a later reset able to finish the job.
        let dirs = match cache_dirs(
            &state.paths.clone().with_cache_root(root.clone()),
            &state.app_db,
        )
        .await
        {
            Ok(dirs) => dirs,
            Err(err) => {
                // The enumeration needs `app.db` to list the profiles,
                // and that is exactly what a reset is in the middle of
                // closing. Skipping the root entirely was the first
                // answer and it is worse than it looks: the caches the
                // user moved off their system drive are the whole
                // reason this function exists, and leaving them is
                // leaving gigabytes behind on a reset that said it
                // erased everything.
                //
                // The names are known without the database.
                // `OWNED_NAMES` is the top level WaveFlow creates under
                // a cache root, `claim` refuses a root where any of
                // them already existed, and the marker checked above
                // says WaveFlow made this tree -- so removing them by
                // name is no broader a claim than the enumeration would
                // have made. Only per-profile *names* are lost, and
                // `profiles/` contains all of them.
                tracing::warn!(
                    %err,
                    root = %root.display(),
                    "could not enumerate relocated cache directories for the reset;                      falling back to the owned names under this root"
                );
                OWNED_NAMES
                    .iter()
                    .map(|name| ((*name).to_string(), root.join(name)))
                    .collect()
            }
        };
        targets.push(RelocatedCache {
            marker: root.join(OWNER_MARKER),
            dirs: dirs.into_iter().map(|(_, path)| path).collect(),
        });
    }
    targets
}
