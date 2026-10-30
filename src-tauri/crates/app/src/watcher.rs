//! Filesystem watcher for `is_watched=1` library folders.
//!
//! Each watched folder gets a [`notify`] `RecommendedWatcher` whose
//! events are funneled (via a sync→async bridge) into a per-folder
//! tokio task. That task debounces bursts (1.5 s of quiet) so saving
//! 30 files at once via Finder/Explorer triggers a single rescan, and
//! invokes the existing [`crate::commands::scan::scan_folder_inner`]
//! pipeline so we don't re-implement the metadata extraction logic.
//!
//! On every successful rescan we emit `library:rescanned` so the UI
//! can refetch the affected library's view without polling.
//!
//! Lifetime:
//! - Watchers are created on demand via [`WatcherManager::watch`] (called
//!   from `set_folder_watched` and on app startup).
//! - Stopping is just dropping the [`FolderHandle`] — `RecommendedWatcher`
//!   tears down its background thread on drop, and the oneshot
//!   shutdown channel signals the debounce task to exit.
//! - Profile switches must call [`WatcherManager::unwatch_all`] then
//!   [`WatcherManager::restore_from_db`] with the new pool.
//! - A folder that cannot be armed — usually a drive that is not plugged
//!   in yet — is not given up on: [`WatcherManager::watch_or_wait`]
//!   keeps a placeholder for it and tries again every
//!   [`REARM_INTERVAL`], then scans it once it is back. The placeholder
//!   is dropped exactly like a watcher, so unwatching or switching
//!   profile stops the retries too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{mpsc, oneshot};

use crate::commands::scan::scan_folder_inner;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Quiet-period before a burst of events triggers a rescan. Chosen so
/// that copying a directory of 100 tracks (which fires 100s of Modify
/// events as the OS finalizes each file) yields a single scan.
const DEBOUNCE: Duration = Duration::from_millis(1500);

/// How often a watched folder that could not be armed is tried again.
/// A drive plugged in after launch is picked up within this long;
/// shorter would only spend more stat calls on a drive that is absent
/// for days.
const REARM_INTERVAL: Duration = Duration::from_secs(60);

/// Shortest gap between two logged errors from one folder's watcher. An
/// error can repeat for every event the backend fails to deliver, and a
/// log flooded with the same line hides everything else in it.
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Tauri event fired after a watcher-driven rescan completes. The
/// frontend should refetch `list_libraries` (which feeds the
/// "library.updated_at" reactivity used by the views).
const EVENT_LIBRARY_RESCANNED: &str = "library:rescanned";

#[derive(Serialize, Clone)]
struct LibraryRescannedPayload {
    library_id: i64,
    folder_id: i64,
    /// `true` when the rescan added or updated at least one track,
    /// so the UI can suppress visual churn on no-op events.
    changed: bool,
}

/// Per-folder state held inside [`WatcherManager`]. Dropping this
/// struct stops both the OS-level watcher and the debounce task — or,
/// for a folder still waiting to be armed, its retry task.
struct FolderHandle {
    /// Held only so the underlying notify thread stays alive — never
    /// read after construction. `None` while the folder is waiting to
    /// be armed (see [`WatcherManager::watch_or_wait`]).
    _watcher: Option<RecommendedWatcher>,
    /// Signals the debounce task (or the retry task) to exit. Sender is
    /// dropped on removal; the task's receiver resolves and it bails.
    _shutdown: oneshot::Sender<()>,
}

/// State container managed by Tauri so the toggle command can mutate
/// the watcher set in place.
pub struct WatcherManager {
    inner: Arc<Mutex<HashMap<i64, FolderHandle>>>,
    app: AppHandle,
}

impl WatcherManager {
    pub fn new(app: AppHandle) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            app,
        }
    }

    /// Start watching `path` for `folder_id`. If a watcher already
    /// existed for this id (e.g. after a path edit) it's dropped first.
    pub fn watch(&self, folder_id: i64, library_id: i64, path: PathBuf) -> AppResult<()> {
        let (handle, _scan) = arm(&self.app, folder_id, library_id, &path)?;
        let mut map = self.inner.lock().expect("watcher map poisoned");
        // Replacing drops the previous handle, which stops its watcher
        // and signals its debounce task — exactly what we want when
        // the same folder is re-armed.
        map.insert(folder_id, handle);
        tracing::info!(folder_id, library_id, path = %path.display(), "folder watcher started");
        Ok(())
    }

    /// [`Self::watch`], and when the folder cannot be armed, keep trying.
    ///
    /// For the callers that act on the user's standing choice rather
    /// than on a click: the folders restored at launch and the toggle.
    /// The usual reason a watched folder cannot be armed is that it is
    /// not there yet — an external drive plugged in after launch, a
    /// share that mounts late — and giving up on it then meant it went
    /// unwatched until the next restart, with nothing to say so.
    pub fn watch_or_wait(&self, folder_id: i64, library_id: i64, path: PathBuf) {
        let Err(err) = self.watch(folder_id, library_id, path.clone()) else {
            return;
        };
        tracing::warn!(
            folder_id,
            path = %path.display(),
            %err,
            "folder watcher not armed; trying again every {}s",
            REARM_INTERVAL.as_secs()
        );
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        // In the map like an armed watcher, so `unwatch`, `unwatch_all`
        // and a later `watch` of the same folder all end the retries the
        // same way they stop a watcher: by dropping this handle.
        self.inner.lock().expect("watcher map poisoned").insert(
            folder_id,
            FolderHandle {
                _watcher: None,
                _shutdown: shutdown_tx,
            },
        );
        tauri::async_runtime::spawn(rearm_loop(
            self.app.clone(),
            Arc::clone(&self.inner),
            shutdown_rx,
            folder_id,
            library_id,
            path,
        ));
    }

    /// Stop watching `folder_id`. No-op when the folder isn't watched.
    pub fn unwatch(&self, folder_id: i64) {
        let mut map = self.inner.lock().expect("watcher map poisoned");
        if map.remove(&folder_id).is_some() {
            tracing::info!(folder_id, "folder watcher stopped");
        }
    }

    /// Stop every watcher. Called on profile switch and app shutdown.
    pub fn unwatch_all(&self) {
        let mut map = self.inner.lock().expect("watcher map poisoned");
        let count = map.len();
        map.clear();
        if count > 0 {
            tracing::info!(count, "stopped all folder watchers");
        }
    }

    /// Boot-time hydration: query every `is_watched=1` folder in the
    /// active profile and arm a watcher for each one. A folder that
    /// cannot be armed is logged and retried on its own (see
    /// [`Self::watch_or_wait`]), so a missing path on one folder neither
    /// disables watching everywhere nor stays unwatched for the session.
    pub async fn restore_from_db(&self, pool: &SqlitePool) -> AppResult<()> {
        let rows: Vec<(i64, i64, String)> =
            sqlx::query_as("SELECT id, library_id, path FROM library_folder WHERE is_watched = 1")
                .fetch_all(pool)
                .await?;

        for (folder_id, library_id, path) in rows {
            self.watch_or_wait(folder_id, library_id, PathBuf::from(&path));
        }
        Ok(())
    }
}

/// Build the OS watcher for `path` and the debounce task behind it.
///
/// Returns the handle to keep in the map, and a sender that asks the
/// debounce task for a scan exactly as an event would.
fn arm(
    app: &AppHandle,
    folder_id: i64,
    library_id: i64,
    path: &Path,
) -> AppResult<(FolderHandle, UnboundedSender<()>)> {
    if !path.is_dir() {
        return Err(AppError::Other(format!(
            "watch path is not a directory: {}",
            path.display()
        )));
    }

    // Bridge the sync notify callback into an async tokio channel.
    // Bounded would risk dropping events under load — unbounded is
    // fine because the debounce stage collapses bursts to a single
    // scan anyway.
    let (event_tx, event_rx) = mpsc::unbounded_channel::<()>();
    let event_tx_for_cb = event_tx.clone();
    let reserved_root = path.to_path_buf();
    let mut errors = ErrorThrottle::default();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let event = match res {
            Ok(event) => event,
            Err(err) => {
                // Logged, not acted on: an error does not say which
                // changes it cost, and lost events are reported as a
                // rescan event instead (see `is_relevant_event`).
                if let Some(suppressed) = errors.admit(Instant::now()) {
                    tracing::warn!(folder_id, %err, suppressed, "folder watcher error");
                }
                return;
            }
        };
        if !is_relevant_event(&event) {
            return;
        }
        if is_our_own_media(&reserved_root, &event) {
            return;
        }
        // Best-effort send — the debounce task may be tearing down.
        let _ = event_tx_for_cb.send(());
    })
    .map_err(|e| AppError::Other(format!("notify watcher init: {e}")))?;

    watcher
        .watch(path, RecursiveMode::Recursive)
        .map_err(|e| AppError::Other(format!("notify watch {}: {e}", path.display())))?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tauri::async_runtime::spawn(debounce_loop(
        event_rx,
        shutdown_rx,
        app.clone(),
        folder_id,
        library_id,
        event_tx.clone(),
    ));

    Ok((
        FolderHandle {
            _watcher: Some(watcher),
            _shutdown: shutdown_tx,
        },
        event_tx,
    ))
}

/// Retry task for a watched folder that could not be armed.
///
/// Every [`REARM_INTERVAL`] it looks for the folder; once it is there
/// and armed, the watcher takes the placeholder's place in the map and
/// one scan is asked for. Changes made while the folder was away — the
/// whole drive's worth, if it comes back with new music on it — never
/// produced an event, so without that scan they would wait for the next
/// one.
///
/// Ends when its placeholder is dropped: the folder was unwatched or
/// removed, the profile switched, or a later `watch` replaced it.
async fn rearm_loop(
    app: AppHandle,
    inner: Arc<Mutex<HashMap<i64, FolderHandle>>>,
    mut shutdown_rx: oneshot::Receiver<()>,
    folder_id: i64,
    library_id: i64,
    path: PathBuf,
) {
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => return,
            _ = tokio::time::sleep(REARM_INTERVAL) => {}
        }
        // Through tokio's blocking pool: a share that went away can take
        // the filesystem's whole timeout to answer a stat.
        let present = tokio::fs::metadata(&path)
            .await
            .is_ok_and(|meta| meta.is_dir());
        if !present {
            continue;
        }
        // On the blocking pool too: arming stats the folder and registers
        // a recursive watch, one per directory under inotify, which on a
        // large collection is not a runtime worker's job.
        let armed = {
            let app = app.clone();
            let path = path.clone();
            tokio::task::spawn_blocking(move || arm(&app, folder_id, library_id, &path)).await
        };
        let (handle, scan) = match armed {
            Ok(Ok(armed)) => armed,
            Ok(Err(err)) => {
                tracing::debug!(folder_id, %err, "folder is back but could not be armed yet");
                continue;
            }
            Err(err) => {
                tracing::warn!(folder_id, %err, "arming the folder watcher panicked");
                continue;
            }
        };
        {
            let mut map = inner.lock().expect("watcher map poisoned");
            // Only if the placeholder is still ours. Every removal from
            // the map happens under this lock and drops the sender with
            // it, so a closed channel here means the folder was
            // unwatched or replaced meanwhile — and the watcher just
            // built is dropped rather than resurrecting it, possibly
            // under another profile's folder of the same id.
            if !matches!(
                shutdown_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ) {
                return;
            }
            map.insert(folder_id, handle);
        }
        tracing::info!(folder_id, library_id, path = %path.display(), "watched folder is back; watching it again");
        let _ = scan.send(());
        return;
    }
}

/// Filter notify events down to the ones that can change the library.
/// We ignore Access (read-only, never mutates content) and
/// AccessKind metadata-only changes.
///
/// Plus the backend's own "events were lost, look again" signal: an
/// inotify queue that overflowed, an FSEvents stream that says a
/// subtree must be rescanned. It comes as `EventKind::Other` with the
/// rescan flag, usually naming no path, and it is precisely what a large
/// copy into the folder produces — dropping it meant that copy was never
/// picked up. It asks for the same debounced scan of the whole folder
/// any other event does.
fn is_relevant_event(event: &Event) -> bool {
    event.need_rescan()
        || matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        )
}

/// Rate limit for the errors one folder's watcher reports.
#[derive(Default)]
struct ErrorThrottle {
    last_logged: Option<Instant>,
    suppressed: u64,
}

impl ErrorThrottle {
    /// `Some(n)` when the error arriving at `now` should be logged, `n`
    /// being how many were swallowed since the last one that was, so
    /// the log still says how often it happened.
    fn admit(&mut self, now: Instant) -> Option<u64> {
        match self.last_logged {
            Some(last) if now.duration_since(last) < ERROR_LOG_INTERVAL => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last_logged = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// Whether every path this event names is media WaveFlow itself keeps in
/// the library folder (issue #695).
///
/// Without this, caching one Canvas clip would spend a full folder rescan
/// answering our own write -- and a prefetch across an album would do it
/// once per clip.
///
/// **Every path, not any**: an event that touches one of our files and one
/// of the user's is a library change, and the user's half is the half that
/// matters. An event that names no path at all cannot be judged, so it is
/// kept -- a scan too many is recoverable, a change never seen is not.
fn is_our_own_media(root: &Path, event: &Event) -> bool {
    !event.paths.is_empty()
        && event
            .paths
            .iter()
            .all(|path| waveflow_core::scanner::is_in_reserved_dir(root, path))
}

/// Per-folder debounce task. Coalesces bursts of events into a single
/// scan after `DEBOUNCE` of quiet time.
///
/// `event_tx` is held inside this function only so its existence
/// pins the channel open even when the notify callback temporarily
/// drops its clone — a defensive measure that has no effect in the
/// current single-callback wiring but documents intent.
async fn debounce_loop(
    mut event_rx: mpsc::UnboundedReceiver<()>,
    mut shutdown_rx: oneshot::Receiver<()>,
    app: AppHandle,
    folder_id: i64,
    library_id: i64,
    event_tx: UnboundedSender<()>,
) {
    let _keepalive = event_tx;

    loop {
        // Wait for the first event of a new burst, with a chance to
        // exit cleanly on shutdown.
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => return,
            msg = event_rx.recv() => {
                if msg.is_none() {
                    return;
                }
            }
        }

        // Accumulate further events until we hit DEBOUNCE of silence.
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => return,
                msg = event_rx.recv() => {
                    if msg.is_none() {
                        return;
                    }
                    // Reset the quiet-period timer.
                }
                _ = tokio::time::sleep(DEBOUNCE) => break,
            }
        }

        run_scan(&app, folder_id, library_id).await;
    }
}

/// Resolve the active profile's pool, run `scan_folder_inner`, then
/// emit `library:rescanned` so the UI can refetch.
async fn run_scan(app: &AppHandle, folder_id: i64, library_id: i64) {
    let state = app.state::<AppState>();
    let (pool, profile_id) = match state.require_profile_snapshot().await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::warn!(folder_id, %err, "watcher: no active profile, skipping scan");
            return;
        }
    };
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // Pass the AppHandle so scan progress events still flow to any
    // open frontend window — fs-watcher rescans can be just as long as
    // a manual rescan when a big folder is touched.
    match scan_folder_inner(&pool, &artwork_dir, folder_id, Some(app), false).await {
        Ok(summary) => {
            let changed = summary.added > 0 || summary.updated > 0 || summary.removed > 0;
            tracing::info!(
                folder_id,
                library_id,
                added = summary.added,
                updated = summary.updated,
                removed = summary.removed,
                "watcher rescan complete"
            );
            let _ = app.emit(
                EVENT_LIBRARY_RESCANNED,
                LibraryRescannedPayload {
                    library_id,
                    folder_id,
                    changed,
                },
            );
            // Stopped from the task bar: don't answer that by
            // starting the analysis sweep. A watcher rescan registers
            // the same cancellable `LibraryScan` task a manual one
            // does, so it is stoppable exactly the same way.
            if summary.added > 0 && !summary.cancelled {
                crate::commands::analysis::maybe_auto_analyze(app);
            }
            // A watched file that changed is one the scanner rewrote,
            // and with it the genre the enrichment engine had filled.
            if !summary.cancelled {
                crate::commands::enrichment::maybe_run(app);
            }
        }
        Err(err) => {
            tracing::warn!(folder_id, %err, "watcher rescan failed");
        }
    }
}

/// Helper for the toggle command: looks up the folder's library + path
/// and arms or disarms the watcher accordingly. Always commits the DB
/// flag regardless of whether the watcher start/stop succeeded —
/// otherwise a transient I/O error would leave the user unable to
/// flip the toggle off.
pub async fn apply_toggle(
    manager: &WatcherManager,
    pool: &SqlitePool,
    folder_id: i64,
    enable: bool,
) -> AppResult<()> {
    if enable {
        let row: Option<(i64, String)> =
            sqlx::query_as("SELECT library_id, path FROM library_folder WHERE id = ?")
                .bind(folder_id)
                .fetch_optional(pool)
                .await?;
        let Some((library_id, path)) = row else {
            return Err(AppError::Other(format!(
                "library_folder {folder_id} not found"
            )));
        };
        // A folder that cannot be armed right now is logged and retried
        // until it can; the DB flag below is still updated so the UI
        // reflects the user's intent.
        manager.watch_or_wait(folder_id, library_id, PathBuf::from(&path));
    } else {
        manager.unwatch(folder_id);
    }

    sqlx::query("UPDATE library_folder SET is_watched = ? WHERE id = ?")
        .bind(if enable { 1 } else { 0 })
        .bind(folder_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
