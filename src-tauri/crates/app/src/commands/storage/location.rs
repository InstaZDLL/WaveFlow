//! The commands: where the caches are, moving them, and the restart that
//! completes a move.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::AppHandle;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

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
        // Two different messages, because "restart to finish it" is not
        // true when the session already fell back: the destination is
        // the drive that was missing at launch, and no restart can
        // complete a copy onto it. The way out is to pick the folder in
        // use right now, which the branch above treats as cancelling
        // the staged move -- so say that instead of sending someone to
        // restart into the same refusal.
        // Scoped to the root that could not be used, the way
        // `get_cache_location` scopes the same question: the flag is set
        // once at startup and never cleared, so a bare `is_some` keeps
        // answering "we fell back" for the rest of the session even
        // after the choice has moved somewhere reachable -- and would
        // then hand out the cancellation instruction to someone whose
        // staged move can in fact be finished by restarting.
        let fell_back_to_here = {
            let chosen = load_cache_root(&state.app_db).await;
            state
                .cache_root_fallback
                .as_ref()
                .is_some_and(|(root, _)| chosen.as_ref() == Some(root))
        };
        return Err(AppError::Other(
            if fell_back_to_here {
                "a cache move is staged for a folder that is not available; choose the folder in use now to cancel it, then pick a new one"
            } else {
                "a cache move is already staged; restart WaveFlow to finish it"
            }
            .into(),
        ));
    }

    // The probes from here to `claim` are synchronous `std::fs` on the
    // async runtime, and they stay that way. Gathering them into one
    // `spawn_blocking` would mean moving the sequence below into a
    // closure -- and that sequence is not arbitrary: `target_existed`
    // is read before `is_usable` creates anything, the containment
    // check runs before `claim` so a refused move leaves no marker,
    // and `marker_existed` is read before the write it rolls back.
    // Three review passes put those in that order, each after getting
    // it wrong. This is one folder the user just picked in a settings
    // dialog, probed with stat and one `read_dir`; the copy that
    // follows, which is the part that takes time, already runs on the
    // blocking pool.
    //
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
        if let Err(err) = claim(&target) {
            // `is_usable` created the folder if it was missing, and a
            // bare `?` here walks out leaving it -- an empty directory
            // appearing in the user's chosen location as the reward for
            // a move that was refused. Same cleanup as the containment
            // refusal above, and the same `remove_dir` rather than
            // `remove_dir_all`: it declines a non-empty directory, so
            // getting the ownership question wrong costs an empty
            // folder and never somebody's files.
            if !target_existed {
                let _ = std::fs::remove_dir(&target);
            }
            return Err(AppError::Other(err));
        }
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
    // And the folders between the target and each destination
    // (`profiles/`, `profiles/<id>/`), which `create_dir_all` makes on the
    // way: left behind after a stop, an empty `profiles/` without the
    // marker is exactly what `claim` refuses, and the next attempt at the
    // same folder would be turned away.
    let created_parents: Vec<PathBuf> = {
        let mut parents: Vec<PathBuf> = Vec::new();
        for (_, to) in &destinations {
            let mut dir = to.parent();
            while let Some(d) = dir.filter(|d| *d != target.as_path() && d.starts_with(&target)) {
                if !d.exists() && !parents.iter().any(|p| p == d) {
                    parents.push(d.to_path_buf());
                }
                dir = d.parent();
            }
        }
        // Deepest first, so each is empty by the time it is removed.
        parents.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        parents
    };

    // Announced to the task bar with a progress measured in files: a
    // library's worth of covers takes minutes on a slow drive, and the
    // card's spinner alone said neither how far it had got nor that it
    // could be stopped. Stopping is between two files, and leads to the
    // same rollback as a failure below — the setting is only written
    // once everything is copied, so a stopped move changes nothing.
    let stop = Arc::new(AtomicBool::new(false));
    let task = crate::tasks::start(
        &app,
        crate::tasks::TaskKind::CacheMove,
        0,
        crate::tasks::cancel_fn({
            let stop = Arc::clone(&stop);
            move || stop.store(true, Ordering::Release)
        }),
    );
    if let Some(task) = task.as_ref() {
        task.detail(target.to_string_lossy());
    }
    let stop_copy = Arc::clone(&stop);
    let copy = tokio::task::spawn_blocking(move || -> Result<(), CopyStop> {
        let total: u64 = plan.iter().map(|(_, from, _)| count_files(from)).sum();
        let done = std::sync::atomic::AtomicU64::new(0);
        if let Some(task) = task.as_ref() {
            task.progress(0, total);
        }
        let on_file = || {
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(task) = task.as_ref() {
                // Held under the total: a file added to a cache while
                // the move runs would otherwise push past it.
                task.progress(n.min(total.saturating_sub(1)), total);
            }
        };
        for (name, from, to) in plan {
            copy_tree(&from, &to, &stop_copy, &on_file).map_err(|e| match e {
                CopyStop::Cancelled => CopyStop::Cancelled,
                CopyStop::Failed(e) => CopyStop::Failed(AppError::Other(format!(
                    "copying {name} to {} failed: {e}",
                    to.display()
                ))),
            })?;
        }
        // A stop pressed while the last file was copying arrives after
        // the last check: the move is not finished until the setting is
        // written, so it is still honoured here.
        if stop_copy.load(Ordering::Acquire) {
            return Err(CopyStop::Cancelled);
        }
        Ok(())
    })
    .await
    .map_err(|e| CopyStop::Failed(AppError::Other(format!("cache copy task failed: {e}"))));

    // A copy that stopped part-way has adopted a folder and filled some
    // of it, and nothing records either: the setting is written further
    // down and only on success, so without this the user is left with a
    // half-copied artwork tree and a marker saying WaveFlow owns the
    // folder -- which is the one file that lets a later reset delete it.
    // Rolled back to exactly what was there before, never further.
    if let Err(stopped) = copy.and_then(|inner| inner) {
        // On the blocking pool: a stopped move can leave thousands of
        // copied files to remove, too long to hold an executor thread.
        let outside_root = target != paths.root;
        let _ = tokio::task::spawn_blocking(move || {
            // A copy that would not go keeps the marker: it is what lets a
            // later attempt at this folder, or a reset, claim the copy back.
            let mut copies_gone = true;
            for ((_, dir), existed) in destinations.iter().zip(pre_existing.iter()) {
                if !existed {
                    if let Err(err) = std::fs::remove_dir_all(dir) {
                        copies_gone &= err.kind() == std::io::ErrorKind::NotFound;
                    }
                }
            }
            // `remove_dir`, which refuses a folder that is not empty: these
            // were made by this attempt, but a file the user dropped in since
            // is still theirs.
            for dir in &created_parents {
                let _ = std::fs::remove_dir(dir);
            }
            if !marker_existed && outside_root && copies_gone {
                let _ = std::fs::remove_file(target.join(OWNER_MARKER));
            }
            // And the folder itself, for the same reason: `is_usable` is
            // what brought it into being, so a failed move must not leave
            // it standing. Last, because it can only go once the copies and
            // the marker inside it are gone.
            if !target_existed {
                let _ = std::fs::remove_dir(&target);
            }
        })
        .await;
        return match stopped {
            // Asked for: nothing moved, which is what the card shows.
            CopyStop::Cancelled => get_cache_location(state).await,
            CopyStop::Failed(err) => Err(err),
        };
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
    crate::skin_recovery::graceful_exit();
    app.restart();
}
