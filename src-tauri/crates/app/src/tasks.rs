//! One place that knows what is running (issue #601).
//!
//! Long operations reported themselves inconsistently: a scan emitted
//! `scan:progress` and the Library view rendered it, an analysis sweep
//! emitted `analysis:progress` and a settings card rendered *that*, and
//! a mirror walk or a backup reported nowhere at all. So the question
//! users actually ask — "why is this machine busy" — had no answer, and
//! somebody who started an analysis over forty thousand tracks and
//! wanted their laptop back had to quit the application.
//!
//! # What this is not
//!
//! **It is not a new cancellation mechanism.** Contrary to the issue's
//! premise, five of these tasks could already be stopped
//! (`cancel_library_analysis`, `cancel_lyrics_prefetch`,
//! `remote_cancel_upload`, `remote_cancel_catalogue_mirror`,
//! `remote_cancel_reconcile_scan`), each with its own notion of a safe
//! stopping point. A registry that re-implemented cancellation would
//! have to re-derive all five, and would get them subtly wrong: a
//! reconciliation may only stop between batches, a mirror walk only
//! between albums, a scan only *before* the pass that marks missing
//! files unavailable. So a task registers a **callback** and the
//! registry routes to it. The safe stopping point stays where it is
//! defined.
//!
//! **It is not a notification centre.** Entries exist while their task
//! runs and vanish when it ends. Nothing here is history.
//!
//! # Lifetime
//!
//! [`TaskHandle`] removes its entry on `Drop`, so every exit path —
//! early return, `?`, panic — retires the row. A task that leaves a
//! ghost in this list is worse than one that reports nothing at all:
//! the user sees a spinner that never resolves and a cancel button that
//! does nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// Event carrying the whole list whenever it changes.
///
/// The full list rather than a delta: it never holds more than a
/// handful of rows, and a delta protocol would need the frontend to
/// reconstruct state from an event stream that Tauri does not replay
/// for a listener registered a moment too late — the failure this
/// codebase has already paid for once (see the "subscribe first, then
/// snapshot" invariant).
pub const TASKS_CHANGED: &str = "tasks:changed";

/// Smallest gap between two progress-driven emissions.
///
/// Start and finish are always emitted immediately; only progress is
/// throttled. A scan ticks every 25 files, which on a warm cache is
/// tens of times a second, and the status bar cannot render that.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(250);

/// What kind of work a row describes.
///
/// The slug is the contract with the frontend, which turns it into
/// localized copy — so these strings are as stable as an event name and
/// must not be renamed casually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    LibraryScan,
    Analysis,
    LyricsPrefetch,
    CatalogueMirror,
    Reconcile,
    Upload,
    Backup,
    Thumbnails,
}

impl TaskKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::LibraryScan => "library_scan",
            Self::Analysis => "analysis",
            Self::LyricsPrefetch => "lyrics_prefetch",
            Self::CatalogueMirror => "catalogue_mirror",
            Self::Reconcile => "reconcile",
            Self::Upload => "upload",
            Self::Backup => "backup",
            Self::Thumbnails => "thumbnails",
        }
    }
}

/// One running task, as the frontend sees it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    pub id: u64,
    /// Stable slug; the frontend maps it to localized copy.
    pub kind: &'static str,
    /// Free text under the label — the folder being scanned, the track
    /// being analysed. Not localized: it is a path or a title.
    pub detail: Option<String>,
    pub current: u64,
    /// `0` means indeterminate. Kept as a number rather than an
    /// `Option` so the two cases have one shape on the wire.
    pub total: u64,
    pub cancellable: bool,
    /// A cancel has been asked for and the task has not exited yet.
    /// The button stays visible but stops accepting clicks — a second
    /// press cannot make a task stop sooner, and a control that looks
    /// live while doing nothing invites exactly that.
    pub cancelling: bool,
}

type CancelFn = Arc<dyn Fn() + Send + Sync>;

/// How a task can be stopped.
///
/// Two cases, and the distinction is not cosmetic: the first version of
/// this had only "a callback or nothing", and the library scan — whose
/// mechanism *is* the registry's own flag — was registered with
/// nothing. [`TaskRegistry::cancel`] returns early when there is no
/// callback, so the flag was never set and the stop button never
/// appeared at all.
///
/// Every long operation in the app turned out to have an honest
/// stopping point once each was looked at, down to the thumbnail pass,
/// which stops between two files. If one ever genuinely has none, a
/// third variant belongs here — and the status bar already renders a
/// row with no button when `cancellable` is false.
pub enum Cancellation {
    /// The task polls [`TaskHandle::is_cancelling`]. Used by work that
    /// had no `cancel_*` command of its own.
    Flag,
    /// The task's existing stopping mechanism. The registry calls it
    /// and nothing else — a reconciliation may only stop between
    /// batches, a mirror walk between albums, an upload after the
    /// current track, and those rules stay where they are defined.
    Callback(CancelFn),
}

struct TaskEntry {
    kind: TaskKind,
    detail: Option<String>,
    current: u64,
    total: u64,
    cancel: Cancellation,
    cancelling: bool,
    /// True until this task's [`TaskHandle`] is dropped. See
    /// [`TaskRegistry::cancel`] for why a bool needs a mutex of its own.
    live: Arc<Mutex<bool>>,
}

impl TaskEntry {
    fn snapshot(&self, id: u64) -> TaskSnapshot {
        TaskSnapshot {
            id,
            kind: self.kind.slug(),
            detail: self.detail.clone(),
            current: self.current,
            total: self.total,
            // Always true today: every long operation turned out to
            // have an honest stopping point. Kept on the wire because
            // the frontend renders a row with no button when it is
            // false, which is what a future task without one needs.
            cancellable: true,
            cancelling: self.cancelling,
        }
    }
}

#[derive(Default)]
struct Inner {
    entries: HashMap<u64, TaskEntry>,
    /// Insertion order, so the status bar does not reshuffle itself
    /// every time a `HashMap` feels like it. A task appearing above or
    /// below the one you were about to cancel is how the wrong thing
    /// gets cancelled.
    order: Vec<u64>,
    last_emit: Option<Instant>,
}

/// The registry itself. Managed by Tauri and cloned freely.
pub struct TaskRegistry {
    inner: Mutex<Inner>,
    /// Held across "take a snapshot, then emit it".
    ///
    /// Without it two threads can snapshot in one order and emit in the
    /// other, so the status bar settles on the *older* list — a task
    /// that has finished stays on screen with a cancel button that does
    /// nothing. A second lock rather than widening `inner`, because
    /// `emit` is arbitrary runtime code and holding the state lock
    /// across it is how a deadlock gets built.
    emit: Mutex<()>,
    next_id: AtomicU64,
    app: AppHandle,
}

impl TaskRegistry {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            emit: Mutex::new(()),
            next_id: AtomicU64::new(1),
            app,
        })
    }

    /// Announce a task and get back the handle that retires it.
    ///
    /// `total` of `0` means "no idea how much there is", which is an
    /// honest answer for a mirror walk whose page count is unknown
    /// until the server has been asked.
    ///
    /// `cancel` says how this task stops — see [`Cancellation`].
    pub fn start(self: &Arc<Self>, kind: TaskKind, total: u64, cancel: Cancellation) -> TaskHandle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let live = Arc::new(Mutex::new(true));
        {
            let mut inner = self.lock();
            inner.entries.insert(
                id,
                TaskEntry {
                    kind,
                    detail: None,
                    current: 0,
                    total,
                    cancel,
                    cancelling: false,
                    live: Arc::clone(&live),
                },
            );
            inner.order.push(id);
        }
        self.emit_now();
        TaskHandle {
            id,
            live,
            registry: Arc::clone(self),
        }
    }

    pub fn snapshot(&self) -> Vec<TaskSnapshot> {
        let inner = self.lock();
        inner
            .order
            .iter()
            .filter_map(|id| inner.entries.get(id).map(|entry| entry.snapshot(*id)))
            .collect()
    }

    /// Ask a task to stop, through its own mechanism.
    ///
    /// Returns whether anything was asked. A row that is already
    /// cancelling answers `false`: the callback has been called, and
    /// calling it again cannot make the task stop sooner.
    pub fn cancel(&self, id: u64) -> bool {
        let found = {
            let mut inner = self.lock();
            let Some(entry) = inner.entries.get_mut(&id) else {
                return false;
            };
            if entry.cancelling {
                return false;
            }
            let callback = match &entry.cancel {
                // The flag *is* the mechanism: setting `cancelling`
                // below is the whole of it, and the task sees it on its
                // next poll.
                Cancellation::Flag => None,
                Cancellation::Callback(f) => Some(Arc::clone(f)),
            };
            entry.cancelling = true;
            callback.map(|callback| (callback, Arc::clone(&entry.live)))
        };
        // Outside the registry's lock: a callback flips an atomic today,
        // but holding that mutex across arbitrary user code is how a
        // deadlock gets built one honest refactor at a time.
        //
        // Under the task's *own* lock, though, and only while the task
        // is still live. The stopping flags these callbacks set are
        // process-wide statics shared by every run of their operation
        // (`ANALYSIS_CANCEL`, `PREFETCH_CANCEL`), and each run clears
        // its flag on the way in. Without this gate, a callback picked
        // up here could fire after the run it belongs to had finished
        // and a *new* one had started — stopping a fresh analysis the
        // user never asked to stop. `Drop` takes this same lock to
        // clear the flag, and neither side ever holds both mutexes at
        // once, so the two can only order, not deadlock.
        if let Some((callback, live)) = found {
            let live = live.lock().unwrap_or_else(|e| e.into_inner());
            if *live {
                callback();
            }
        }
        self.emit_now();
        true
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock here means a panic happened while a row was
        // being edited. The rows are plain data with no invariant
        // spanning them, so carrying on with the contents is strictly
        // better than taking the whole app down over a progress list.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit_now(&self) {
        let _order = self.emit.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut inner = self.lock();
            inner.last_emit = Some(Instant::now());
        }
        let _ = self.app.emit(TASKS_CHANGED, self.snapshot());
    }

    /// Emit unless one went out very recently. Progress only.
    fn emit_throttled(&self) {
        let _order = self.emit.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut inner = self.lock();
            if inner
                .last_emit
                .is_some_and(|at| at.elapsed() < PROGRESS_THROTTLE)
            {
                return;
            }
            inner.last_emit = Some(Instant::now());
        }
        let _ = self.app.emit(TASKS_CHANGED, self.snapshot());
    }

    fn finish(&self, id: u64) {
        {
            let mut inner = self.lock();
            inner.entries.remove(&id);
            inner.order.retain(|other| *other != id);
        }
        self.emit_now();
    }
}

/// Keeps one task's row alive. Dropping it retires the row.
pub struct TaskHandle {
    id: u64,
    /// Set to `false` before the row is retired, so a cancellation that
    /// is already on its way cannot land on the next run. See
    /// [`TaskRegistry::cancel`].
    live: Arc<Mutex<bool>>,
    registry: Arc<TaskRegistry>,
}

impl TaskHandle {
    /// Update the counter. The final emission is *not* throttled, so a
    /// task that ends on a full bar shows a full bar rather than
    /// whatever the last throttled tick happened to say.
    pub fn progress(&self, current: u64, total: u64) {
        let complete = total > 0 && current >= total;
        {
            let mut inner = self.registry.lock();
            let Some(entry) = inner.entries.get_mut(&self.id) else {
                return;
            };
            entry.current = current;
            entry.total = total;
        }
        if complete {
            self.registry.emit_now();
        } else {
            self.registry.emit_throttled();
        }
    }

    /// Set the line under the label. Throttled like progress — it
    /// changes at the same rate and for the same reason.
    pub fn detail(&self, detail: impl Into<String>) {
        {
            let mut inner = self.registry.lock();
            let Some(entry) = inner.entries.get_mut(&self.id) else {
                return;
            };
            entry.detail = Some(detail.into());
        }
        self.registry.emit_throttled();
    }

    /// Has someone pressed cancel?
    ///
    /// For a task whose stopping mechanism is its own flag this is
    /// redundant — it will see its flag. It exists for the scan, which
    /// had no mechanism of its own before this.
    pub fn is_cancelling(&self) -> bool {
        self.registry
            .lock()
            .entries
            .get(&self.id)
            .is_some_and(|entry| entry.cancelling)
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        // Before the row goes, and in its own scope: the guard must be
        // released before `finish` takes the registry's lock, or the two
        // orders would close a cycle with `cancel`.
        {
            let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
            *live = false;
        }
        self.registry.finish(self.id);
    }
}

/// Announce a task from anywhere that has an `AppHandle`.
///
/// Returns `None` when the registry is not managed yet — which happens
/// during setup, and in the handful of tests that build a bare app. A
/// task that cannot announce itself still runs; it simply does not
/// appear in the status bar, which is the right failure for a progress
/// surface.
pub fn start(
    app: &AppHandle,
    kind: TaskKind,
    total: u64,
    cancel: Cancellation,
) -> Option<TaskHandle> {
    use tauri::Manager;
    app.try_state::<Arc<TaskRegistry>>()
        .map(|registry| registry.inner().start(kind, total, cancel))
}

/// Wrap a plain `fn()` stopping function into a [`Cancellation`].
/// Exists so a call site reads `cancel_fn(request_cancel)` rather than
/// a turbofished `Arc::new`.
pub fn cancel_fn<F>(f: F) -> Cancellation
where
    F: Fn() + Send + Sync + 'static,
{
    Cancellation::Callback(Arc::new(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordering guarantee, without a Tauri app: `order` is what the
    /// status bar renders, and a `HashMap`'s iteration order would make
    /// rows swap places between two ticks.
    #[test]
    fn rows_keep_their_insertion_order() {
        let mut inner = Inner::default();
        for (id, kind) in [
            (1u64, TaskKind::LibraryScan),
            (2, TaskKind::Analysis),
            (3, TaskKind::Backup),
        ] {
            inner.entries.insert(
                id,
                TaskEntry {
                    kind,
                    detail: None,
                    current: 0,
                    total: 0,
                    cancel: Cancellation::Flag,
                    cancelling: false,
                    live: Arc::new(Mutex::new(true)),
                },
            );
            inner.order.push(id);
        }
        inner.entries.remove(&2);
        inner.order.retain(|id| *id != 2);

        let slugs: Vec<&str> = inner
            .order
            .iter()
            .filter_map(|id| inner.entries.get(id).map(|e| e.kind.slug()))
            .collect();
        assert_eq!(slugs, vec!["library_scan", "backup"]);
    }

    /// A kind's slug is a wire contract with the frontend's i18n keys.
    /// Pinned here so renaming one is a deliberate act with a failing
    /// test, not a refactor that silently unlabels a row.
    #[test]
    fn slugs_are_stable_and_distinct() {
        let all = [
            TaskKind::LibraryScan,
            TaskKind::Analysis,
            TaskKind::LyricsPrefetch,
            TaskKind::CatalogueMirror,
            TaskKind::Reconcile,
            TaskKind::Upload,
            TaskKind::Backup,
            TaskKind::Thumbnails,
        ];
        let mut seen = std::collections::HashSet::new();
        for kind in all {
            assert!(seen.insert(kind.slug()), "duplicate slug {}", kind.slug());
        }
        assert_eq!(seen.len(), 8);
        assert_eq!(TaskKind::LibraryScan.slug(), "library_scan");
        assert_eq!(TaskKind::CatalogueMirror.slug(), "catalogue_mirror");
    }
}
