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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
/// struct stops both the OS-level watcher and the debounce task.
struct FolderHandle {
    /// Held only so the underlying notify thread stays alive — never
    /// read after construction.
    _watcher: RecommendedWatcher,
    /// Signals the debounce task to exit. Sender is dropped on
    /// removal; the task's `recv()` returns `Err(_)` and bails.
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
    pub fn watch(
        &self,
        folder_id: i64,
        library_id: i64,
        path: PathBuf,
    ) -> AppResult<()> {
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
        let watcher_path = path.clone();

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(event) = res else { return };
            if !is_relevant_event(&event) {
                return;
            }
            // Best-effort send — the debounce task may be tearing down.
            let _ = event_tx_for_cb.send(());
        })
        .map_err(|e| AppError::Other(format!("notify watcher init: {e}")))?;

        watcher
            .watch(&watcher_path, RecursiveMode::Recursive)
            .map_err(|e| {
                AppError::Other(format!(
                    "notify watch {}: {e}",
                    watcher_path.display()
                ))
            })?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let app = self.app.clone();
        tauri::async_runtime::spawn(debounce_loop(
            event_rx,
            shutdown_rx,
            app,
            folder_id,
            library_id,
            event_tx,
        ));

        let handle = FolderHandle {
            _watcher: watcher,
            _shutdown: shutdown_tx,
        };

        let mut map = self.inner.lock().expect("watcher map poisoned");
        // Replacing drops the previous handle, which stops its watcher
        // and signals its debounce task — exactly what we want when
        // the same folder is re-armed.
        map.insert(folder_id, handle);
        tracing::info!(folder_id, library_id, path = %path.display(), "folder watcher started");
        Ok(())
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
    /// active profile and arm a watcher for each one. Failures are
    /// logged per-folder so a missing path on one folder doesn't
    /// disable watching everywhere.
    pub async fn restore_from_db(&self, pool: &SqlitePool) -> AppResult<()> {
        let rows: Vec<(i64, i64, String)> = sqlx::query_as(
            "SELECT id, library_id, path FROM library_folder WHERE is_watched = 1",
        )
        .fetch_all(pool)
        .await?;

        for (folder_id, library_id, path) in rows {
            if let Err(err) = self.watch(folder_id, library_id, PathBuf::from(&path)) {
                tracing::warn!(folder_id, path, %err, "failed to restore watcher");
            }
        }
        Ok(())
    }
}

/// Filter notify events down to the ones that can change the library.
/// We ignore Access (read-only, never mutates content) and
/// AccessKind metadata-only changes.
fn is_relevant_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
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
    let pool = match state.require_profile_pool().await {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(folder_id, %err, "watcher: no profile pool, skipping scan");
            return;
        }
    };
    let profile_id = match state.require_profile_id().await {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(folder_id, %err, "watcher: no profile id, skipping scan");
            return;
        }
    };
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    match scan_folder_inner(&pool, &artwork_dir, folder_id).await {
        Ok(summary) => {
            let changed =
                summary.added > 0 || summary.updated > 0 || summary.removed > 0;
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
            if summary.added > 0 {
                crate::commands::analysis::maybe_auto_analyze(app);
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
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT library_id, path FROM library_folder WHERE id = ?",
        )
        .bind(folder_id)
        .fetch_optional(pool)
        .await?;
        let Some((library_id, path)) = row else {
            return Err(AppError::Other(format!(
                "library_folder {folder_id} not found"
            )));
        };
        if let Err(err) = manager.watch(folder_id, library_id, PathBuf::from(&path)) {
            // Surface the error but the DB flag below is still
            // updated so the UI reflects the user's intent.
            tracing::warn!(folder_id, path, %err, "watch start failed");
        }
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

