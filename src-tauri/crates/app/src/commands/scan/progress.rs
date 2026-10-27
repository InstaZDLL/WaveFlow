//! The scan's bookkeeping: the in-flight count the analyzer parks behind,
//! the progress events, the persisted counters and the summary.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Serialize;
use tauri::Emitter;

use waveflow_core::scanner::now_millis;

/// Process-wide count of in-flight library scans. Bumped for the whole
/// body of [`scan_folder_inner`] — which every scan entry point routes
/// through (`scan_folder`, `rescan_library`, `import_dropped`, the
/// fs-watcher, the startup rescan in `startup/setup.rs`).
///
/// The background library analyzer reads this via [`scan_in_flight`]
/// and parks itself while a scan is running: a scan is foreground (the
/// user watches its progress toast), saturates every CPU core through
/// the parallel extraction pipeline, and hammers the single SQLite
/// writer. Letting the analyzer's CPU-heavy decode + per-track writes
/// run through it both inflates the scan's wall-clock (a clean ~34 s
/// AAC scan ballooned past a minute under a concurrent analysis pass —
/// measured) and loses analysis rows to lock contention. See
/// [`crate::commands::analysis::run_analyze_library`].
static SCANS_IN_FLIGHT: AtomicU32 = AtomicU32::new(0);

/// `true` while at least one library scan is walking + writing.
pub(crate) fn scan_in_flight() -> bool {
    SCANS_IN_FLIGHT.load(Ordering::Acquire) > 0
}

/// RAII guard that bumps [`SCANS_IN_FLIGHT`] for the lifetime of a scan
/// and decrements it on every exit path (early `?` return, error,
/// panic). One is held at the top of [`scan_folder_inner`].
pub(super) struct ScanInFlightGuard;

impl ScanInFlightGuard {
    pub(super) fn new() -> Self {
        SCANS_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ScanInFlightGuard {
    fn drop(&mut self) {
        SCANS_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Payload of the `scan:progress` Tauri event. Emitted by
/// `scan_folder_inner` every ~25 processed files (and once at the end)
/// so the frontend can render a non-blocking toast with the current
/// state of the scan instead of leaving the user staring at a frozen
/// UI for half a minute.
#[derive(Clone, Serialize)]
pub struct ScanProgress {
    pub folder_id: i64,
    pub current: usize,
    pub total: usize,
    pub added: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    pub done: bool,
    /// The scan stopped because the user asked it to (#601). Carried
    /// beside `done` rather than folded into it: the run *is* over, and
    /// what it managed to write is committed and correct, but a toast
    /// reading "scan complete" over a full bar would be claiming the
    /// library was walked when it was not.
    pub cancelled: bool,
    /// Absolute path of the directory the scan is currently in — the
    /// parent of the file just processed. Lets the toast show a live
    /// "scanning …/Album" line instead of a bare counter (#430). `None`
    /// on the initial + final ticks, which carry no specific file.
    pub current_dir: Option<String>,
}

/// Emit a progress tick — best-effort, errors are swallowed because
/// progress feedback should never abort a scan. `current_path` is the
/// file that was just processed; its parent directory rides along so the
/// UI can name the folder being scanned.
pub(super) fn maybe_emit_progress(
    app: Option<&tauri::AppHandle>,
    folder_id: i64,
    current: usize,
    total: usize,
    summary: &ScanSummary,
    current_path: Option<&Path>,
) {
    let Some(app) = app else { return };
    // Ticks every 25 files cap the event volume at ~40 events/s on a
    // hot CPU — enough to feel live without flooding the IPC channel.
    if current != total && current % 25 != 0 {
        return;
    }
    let current_dir = current_path
        .and_then(Path::parent)
        .map(|d| d.to_string_lossy().into_owned());
    let _ = app.emit(
        "scan:progress",
        ScanProgress {
            folder_id,
            current,
            total,
            added: summary.added,
            updated: summary.updated,
            skipped: summary.skipped,
            errors: summary.errors,
            done: false,
            cancelled: false,
            current_dir,
        },
    );
}

/// Read a small integer counter out of `profile_setting`.
///
/// A missing or unreadable row answers `0`: the counter only ever
/// shortens a retry loop, so failing to read it must cost another
/// attempt rather than end the loop early.
pub(super) async fn read_counter(pool: &sqlx::SqlitePool, key: &str, folder_id: i64) -> i64 {
    match sqlx::query_scalar::<_, String>("SELECT value FROM profile_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
    {
        Ok(value) => value.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0),
        Err(err) => {
            tracing::warn!(?err, folder_id, %key, "could not read a backfill counter");
            0
        }
    }
}

pub(super) async fn write_counter(
    pool: &sqlx::SqlitePool,
    key: &str,
    value: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'int', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value.to_string())
    .bind(now_millis())
    .execute(pool)
    .await
    .map(|_| ())
}

/// Has this folder's one-off pass never run?
///
/// A read that fails answers "never run", which forces the pass again —
/// the safe side, since the alternative is recording work that did not
/// happen. But it is logged rather than swallowed: a marker that cannot
/// be read makes every future scan re-read the whole folder, and that
/// looks exactly like the fast path being broken for no reason.
pub(super) async fn marker_absent(pool: &sqlx::SqlitePool, key: &str, folder_id: i64) -> bool {
    match sqlx::query_scalar::<_, String>("SELECT value FROM profile_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
    {
        Ok(value) => value.is_none(),
        Err(err) => {
            tracing::warn!(
                ?err,
                folder_id,
                %key,
                "could not read a backfill marker; running the pass again"
            );
            true
        }
    }
}

/// Outcome of a `scan_folder` call, returned to the frontend so the UI can
/// display a toast like "120 nouveaux titres · 3 mises à jour · 1 erreur".
#[derive(Debug, Serialize, Default)]
pub struct ScanSummary {
    pub folder_id: i64,
    pub scanned: u32,
    pub added: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    /// Tracks marked `is_available = 0` because their file vanished
    /// from disk between scans. The row stays around (and keeps its
    /// liked / playlist / play-event history) so the user can recover
    /// it by putting the file back.
    pub removed: u32,
    /// The user stopped this scan (issue #601). Everything written
    /// before the stop is committed and correct; what the walk had not
    /// reached yet is simply untouched, and the next scan picks it up.
    pub cancelled: bool,
}
