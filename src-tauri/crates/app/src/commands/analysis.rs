//! Tauri commands wrapping the per-track audio analysis.
//!
//! The work itself lives in `crate::analysis`; this layer just bridges
//! the UI to the analyzer:
//!
//! - `analyze_track`: runs the full analysis on one track inside
//!   `spawn_blocking` so the symphonia decode doesn't stall the
//!   tokio runtime, persists the result into `track_analysis`, and
//!   returns it to the caller.
//! - `get_track_analysis`: cheap lookup, used by the Properties
//!   dialog to show pre-computed values without re-running the
//!   analysis on every open.
//! - `analyze_library`: iterates over every available track that
//!   doesn't have a `track_analysis` row yet, emitting progress
//!   events the UI can wire to a progress bar.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    analysis::{analyze_file, AnalysisResult},
    error::{AppError, AppResult},
    state::AppState,
};

const AUTO_ANALYZE_KEY: &str = "audio.auto_analyze";

/// Mutual-exclusion + cancellation primitives for the library-wide
/// analyzer. Mirrors the `PREFETCH_RUNNING` / `PREFETCH_CANCEL` pair
/// in [`crate::commands::lyrics`] — the run is process-wide (only one
/// active profile at a time), so a bare static atomic pair is enough;
/// no need to thread tokens through `AppState`.
///
/// `ANALYSIS_RUNNING` guards against double-start. The user is the
/// one likely to trigger this by clicking the Library "Run analysis"
/// button while the post-scan `maybe_auto_analyze` hook is already in
/// flight — without the guard both runs would race on the same
/// `INSERT … ON CONFLICT` against `track_analysis`, each redoing the
/// same expensive Symphonia decode (issue #286 made this concrete:
/// 1500+ tracks decoded twice would saturate even a beefy laptop).
///
/// `ANALYSIS_CANCEL` is the cooperative stop flag. The loop checks it
/// at every iteration and exits early; the post-loop summary carries
/// `cancelled = true` so the frontend can render "Cancelled at X / Y"
/// instead of pretending the run completed.
static ANALYSIS_RUNNING: AtomicBool = AtomicBool::new(false);
static ANALYSIS_CANCEL: AtomicBool = AtomicBool::new(false);

/// Sleep duration injected between two consecutive `analyze_file`
/// calls in [`run_analyze_library`]. The analyzer is CPU-bound on
/// the Symphonia decode (full audio decode of every sample); on a
/// weak / mid-tier laptop a back-to-back run with zero pauses keeps
/// at least one core pinned at 100% for the entire library, which
/// triggers Windows' thermal throttling and — per issue #286 — can
/// actually freeze the machine when the OS scheduler stops finding
/// idle slices to service the UI and Defender's per-file open scan.
///
/// 25 ms is short enough that a 4000-track library only adds 100 s
/// of pure idle time (~2 % of the analysis wall-clock on a typical
/// 8 ms-per-decode-second track) but long enough to give the OS
/// scheduler room to interleave UI, audio playback, and background
/// services. Combined with `tokio::task::yield_now` it also lets
/// any concurrently-running Tauri command (other than another
/// analyze) progress between tracks.
const ANALYSIS_PER_TRACK_PAUSE: Duration = Duration::from_millis(25);

/// How many decoded results to buffer before flushing them to
/// `track_analysis` in one transaction. Batching collapses N separate
/// write-lock acquisitions into one — on a single-writer WAL database
/// that's the difference between fighting a concurrent writer N times
/// and fighting it once. 16 keeps the crash-loss window small (≈ two
/// minutes of decode at ~8 s/track) while still cutting lock churn 16×.
const ANALYSIS_BATCH_SIZE: usize = 16;

/// Poll interval used by [`wait_out_active_scan`] while parking the
/// analyzer behind an in-flight library scan. Coarse on purpose — a
/// scan runs for tens of seconds, so a quarter-second granularity on
/// noticing it finished is invisible and keeps the wait near-idle.
const SCAN_WAIT_POLL: Duration = Duration::from_millis(250);

/// Row shape returned by `get_track_analysis` and `analyze_track`.
/// Mirrors the columns of `track_analysis` but exposes the fields
/// the UI cares about — `analyzed_at` so a stale-warning ribbon can
/// surface on very old analyses, BPM / loudness / replay gain / peak
/// for the Properties dialog.
#[derive(Debug, Clone, Serialize)]
pub struct TrackAnalysisRow {
    pub track_id: i64,
    pub bpm: Option<f64>,
    pub musical_key: Option<String>,
    pub loudness_lufs: Option<f64>,
    pub replay_gain_db: Option<f64>,
    pub peak: Option<f64>,
    pub analyzed_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryAnalysisProgress {
    pub processed: u32,
    pub total: u32,
    pub current_track_id: Option<i64>,
    pub failed: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryAnalysisSummary {
    pub processed: u32,
    pub failed: u32,
    pub skipped: u32,
    /// `true` when the loop exited because the user clicked the
    /// `cancel_library_analysis` command (or any other call site set
    /// the cancel flag) — the UI uses this to render "Cancelled at
    /// X / Y" instead of pretending the run completed. Also `true`
    /// when [`ANALYSIS_RUNNING`] was already set on entry, since the
    /// second caller never actually ran.
    pub cancelled: bool,
}

/// Look up the cached analysis for a track. Returns `None` when the
/// row doesn't exist yet — the UI uses that to decide whether to
/// show the spec values directly or an "Analyze" button instead.
#[tauri::command]
pub async fn get_track_analysis(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<TrackAnalysisRow>> {
    let pool = state.require_profile_pool().await?;
    // sqlx row tuple — kept anonymous because it's only used here.
    #[allow(clippy::type_complexity)]
    let row: Option<(
        i64,
        Option<f64>,
        Option<String>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        i64,
    )> = sqlx::query_as(
        "SELECT track_id, bpm, musical_key, loudness_lufs, replay_gain_db,
                peak, analyzed_at
           FROM track_analysis
          WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(&pool)
    .await?;
    Ok(row.map(
        |(track_id, bpm, key, loudness, replay, peak, analyzed_at)| TrackAnalysisRow {
            track_id,
            bpm,
            musical_key: key,
            loudness_lufs: loudness,
            replay_gain_db: replay,
            peak,
            analyzed_at,
        },
    ))
}

/// Run the full analysis on one track (decoding the file end-to-end)
/// and persist the result. Returns the freshly-computed row so the
/// caller doesn't need a follow-up `get_track_analysis` call.
#[tauri::command]
pub async fn analyze_track(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<TrackAnalysisRow> {
    let pool = state.require_profile_pool().await?;
    let path: Option<String> = sqlx::query_scalar("SELECT file_path FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&pool)
        .await?;
    let path = path.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;
    let path_buf = PathBuf::from(path);

    // Decode is blocking + CPU-bound; keep it off the tokio reactor.
    let result: AnalysisResult = tokio::task::spawn_blocking(move || analyze_file(&path_buf))
        .await
        .map_err(|e| AppError::Other(format!("analysis task panicked: {e}")))?
        .map_err(AppError::Other)?;

    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO track_analysis
            (track_id, bpm, musical_key, loudness_lufs, replay_gain_db,
             peak, analyzed_at)
         VALUES (?, ?, NULL, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            bpm            = excluded.bpm,
            loudness_lufs  = excluded.loudness_lufs,
            replay_gain_db = excluded.replay_gain_db,
            peak           = excluded.peak,
            analyzed_at    = excluded.analyzed_at",
    )
    .bind(track_id)
    .bind(result.bpm)
    .bind(result.loudness_db)
    .bind(result.replay_gain_db)
    .bind(result.peak)
    .bind(now)
    .execute(&pool)
    .await?;

    Ok(TrackAnalysisRow {
        track_id,
        bpm: result.bpm,
        musical_key: None,
        loudness_lufs: Some(result.loudness_db),
        replay_gain_db: Some(result.replay_gain_db),
        peak: Some(result.peak),
        analyzed_at: now,
    })
}

/// Walk every available track that hasn't been analyzed yet, run the
/// analyzer and persist results. Emits `analysis:progress` events so
/// the UI can drive a progress bar; returns a summary at the end.
#[tauri::command]
pub async fn analyze_library(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<LibraryAnalysisSummary> {
    let pool = state.require_profile_pool().await?;
    run_analyze_library(&app, &pool).await
}

/// Inner worker shared by the user-triggered command and the
/// auto-analyze hook fired after a scan. Takes the pool directly so
/// the caller can decide whether to spawn or await.
///
/// **Cancellation**: the loop checks [`ANALYSIS_CANCEL`] at the top
/// of each iteration and exits early with `cancelled = true` in the
/// summary. The frontend hooks the `cancel_library_analysis` command
/// to a "Stop" button so the user can recover from a run that's
/// pegging their CPU (issue #286 — pre-fix, a 4000-track library
/// would lock a mid-tier laptop for 30+ minutes with no escape).
///
/// **Throttling**: every iteration ends with a `yield_now` + 25 ms
/// sleep ([`ANALYSIS_PER_TRACK_PAUSE`]). The yield gives other tokio
/// tasks (UI events, playback commands, sync drain) a chance to
/// progress between decodes; the sleep gives the OS scheduler room
/// to interleave Windows / macOS / Linux services like Defender's
/// per-file open scan. Adds ~25 ms per track to wall-clock time, so
/// a 4000-track run pays 100 s of pure idle — a 2 % overhead vs the
/// freeze risk it prevents.
pub async fn run_analyze_library(
    app: &AppHandle,
    pool: &SqlitePool,
) -> AppResult<LibraryAnalysisSummary> {
    // Double-start guard. `swap` atomically takes the slot if it was
    // free; if `prev == true` another call is already in flight and
    // we bail with a cancelled summary so the caller's progress UI
    // doesn't get stuck spinning.
    if ANALYSIS_RUNNING.swap(true, Ordering::SeqCst) {
        tracing::info!("analyze_library called while another run is in flight; ignoring");
        return Ok(LibraryAnalysisSummary {
            processed: 0,
            failed: 0,
            skipped: 0,
            cancelled: true,
        });
    }
    // Reset the cancel flag at the START of every run — a stale
    // `true` from a prior cancellation would otherwise short-circuit
    // the new run before the first track. The RAII guard at the end
    // clears `ANALYSIS_RUNNING` on every exit path (early returns +
    // panics) so the flag can't get stuck `true`.
    ANALYSIS_CANCEL.store(false, Ordering::SeqCst);
    let _guard = RunningGuard;

    let pending: Vec<(i64, String)> = sqlx::query_as(
        "SELECT t.id, t.file_path
           FROM track t
           LEFT JOIN track_analysis ta ON ta.track_id = t.id
          WHERE t.is_available = 1 AND ta.track_id IS NULL
          ORDER BY t.id",
    )
    .fetch_all(pool)
    .await?;

    let total = pending.len() as u32;
    let mut processed = 0u32;
    let mut failed = 0u32;
    let mut cancelled = false;
    // Decoded results buffered until `ANALYSIS_BATCH_SIZE`, then
    // flushed in one transaction (see `flush_analysis_batch`).
    let mut batch: Vec<PendingAnalysis> = Vec::with_capacity(ANALYSIS_BATCH_SIZE);

    for (track_id, file_path) in pending {
        // Cancellation gate at the TOP of the loop so a user click
        // that lands between two decodes is honoured without burning
        // one extra ~8-second decode.
        if ANALYSIS_CANCEL.load(Ordering::Relaxed) {
            cancelled = true;
            tracing::info!(processed, total, "library analysis cancelled by user");
            break;
        }

        // Yield to any in-flight foreground scan before the CPU-heavy
        // decode + DB write. `wait_out_active_scan` also returns early
        // on cancel, so re-check before doing real work.
        wait_out_active_scan().await;
        if ANALYSIS_CANCEL.load(Ordering::Relaxed) {
            cancelled = true;
            tracing::info!(processed, total, "library analysis cancelled by user");
            break;
        }

        let _ = app.emit(
            "analysis:progress",
            LibraryAnalysisProgress {
                processed,
                total,
                current_track_id: Some(track_id),
                failed,
            },
        );

        let path_buf = PathBuf::from(file_path);
        let join = tokio::task::spawn_blocking(move || analyze_file(&path_buf)).await;
        match join {
            Ok(Ok(result)) => {
                batch.push(PendingAnalysis {
                    track_id,
                    result,
                    analyzed_at: Utc::now().timestamp_millis(),
                });
                if batch.len() >= ANALYSIS_BATCH_SIZE {
                    failed += flush_analysis_batch(pool, &mut batch).await;
                }
            }
            Ok(Err(err)) => {
                tracing::warn!(track_id, %err, "analyze track failed");
                failed += 1;
            }
            Err(err) => {
                tracing::warn!(track_id, %err, "analyze task panicked");
                failed += 1;
            }
        }
        processed += 1;

        // Cooperative scheduling pair: yield to give other tokio
        // tasks (UI events, drain ticks, playback commands) a turn
        // on the reactor, then sleep to free the OS scheduler so a
        // background-priority service (Defender, Spotlight, etc.)
        // can run between two CPU-bound decodes. See the doc on
        // `ANALYSIS_PER_TRACK_PAUSE` for the cost / benefit
        // breakdown.
        tokio::task::yield_now().await;
        tokio::time::sleep(ANALYSIS_PER_TRACK_PAUSE).await;
    }

    // Persist whatever's left in the buffer — both the normal end of
    // the run and the cancel `break` above land here, so a cancelled
    // run still saves every track it had already decoded.
    failed += flush_analysis_batch(pool, &mut batch).await;

    let summary = LibraryAnalysisSummary {
        processed,
        failed,
        // Currently nothing is skipped — the WHERE filter already
        // excludes already-analyzed rows. Reserved for a future
        // "skip if older than N days" option.
        skipped: 0,
        cancelled,
    };
    let _ = app.emit(
        "analysis:progress",
        LibraryAnalysisProgress {
            processed,
            total,
            current_track_id: None,
            failed: summary.failed,
        },
    );
    Ok(summary)
}

/// RAII guard that clears [`ANALYSIS_RUNNING`] on drop. Lets every
/// exit path of `run_analyze_library` — early return on cancel, the
/// SQL error path, a future `?` on a new query, even a panic during
/// decode — reset the running flag without explicit cleanup. Without
/// this a single error from `sqlx::query_as` would leave the static
/// `true` forever and brick auto-analyze for the rest of the
/// session.
struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        ANALYSIS_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// One decoded-but-not-yet-persisted analysis result, buffered until
/// the batch reaches [`ANALYSIS_BATCH_SIZE`] (or the run ends).
struct PendingAnalysis {
    track_id: i64,
    result: AnalysisResult,
    analyzed_at: i64,
}

/// `true` when a sqlx error is a SQLite busy/locked contention — i.e.
/// the single-writer WAL lock was held by another writer (a concurrent
/// scan, a `play_event` insert) — rather than a real schema/constraint
/// fault. Only the former is worth retrying.
fn is_busy(err: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db) = err {
        if let Some(code) = db.code() {
            return code_is_busy(&code);
        }
    }
    false
}

/// Pure classifier for a SQLite result code string (as surfaced by
/// `DatabaseError::code`). SQLite's primary result code lives in the
/// low 8 bits; the high bits carry the extended detail (e.g. BUSY = 5,
/// BUSY_SNAPSHOT = 517, LOCKED = 6, LOCKED_SHAREDCACHE = 262). We mask
/// to the primary so every flavour of busy/locked is caught.
fn code_is_busy(code: &str) -> bool {
    code.parse::<i32>()
        .map(|n| matches!(n & 0xff, 5 | 6))
        .unwrap_or(false)
}

/// Persist one buffered batch inside a single transaction.
async fn persist_batch_once(
    pool: &SqlitePool,
    batch: &[PendingAnalysis],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for p in batch {
        sqlx::query(
            "INSERT INTO track_analysis
                (track_id, bpm, musical_key, loudness_lufs,
                 replay_gain_db, peak, analyzed_at)
             VALUES (?, ?, NULL, ?, ?, ?, ?)
             ON CONFLICT(track_id) DO UPDATE SET
                bpm = excluded.bpm,
                loudness_lufs = excluded.loudness_lufs,
                replay_gain_db = excluded.replay_gain_db,
                peak = excluded.peak,
                analyzed_at = excluded.analyzed_at",
        )
        .bind(p.track_id)
        .bind(p.result.bpm)
        .bind(p.result.loudness_db)
        .bind(p.result.replay_gain_db)
        .bind(p.result.peak)
        .bind(p.analyzed_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Flush a batch of analysis results, retrying the whole batch on a
/// `SQLITE_BUSY` / `SQLITE_LOCKED` collision with exponential backoff
/// so a transient lock (a concurrent scan write, a `play_event`
/// insert) never silently drops freshly-computed BPM / loudness — the
/// pre-fix bug where a per-row `INSERT` hit `database is locked` after
/// the 5 s busy-timeout and the result was lost (issue: analysis vs
/// scan contention). Returns the number of rows that could NOT be
/// persisted after exhausting the retry budget, for the caller's
/// `failed` tally. Always empties `batch` so the buffer is reusable.
async fn flush_analysis_batch(pool: &SqlitePool, batch: &mut Vec<PendingAnalysis>) -> u32 {
    if batch.is_empty() {
        return 0;
    }
    const MAX_ATTEMPTS: usize = 6;
    let mut backoff = Duration::from_millis(50);
    for attempt in 1..=MAX_ATTEMPTS {
        match persist_batch_once(pool, batch).await {
            Ok(()) => {
                batch.clear();
                return 0;
            }
            Err(e) if is_busy(&e) && attempt < MAX_ATTEMPTS => {
                tracing::debug!(attempt, rows = batch.len(), "analysis batch busy; retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(2));
            }
            Err(e) => {
                let dropped = batch.len() as u32;
                tracing::warn!(
                    ?e,
                    rows = dropped,
                    "persist analysis batch failed; dropping"
                );
                batch.clear();
                return dropped;
            }
        }
    }
    // Unreachable: the final attempt either commits or falls into the
    // catch-all `Err` arm above (its guard requires `attempt <
    // MAX_ATTEMPTS`). Kept for the type-checker.
    let dropped = batch.len() as u32;
    batch.clear();
    dropped
}

/// Park the analyzer while a library scan is walking + writing. The
/// scan is foreground work the user is watching; it pins every CPU
/// core through the parallel extraction pipeline and holds the single
/// SQLite writer in bursts. Decoding + writing through it inflates the
/// scan's wall-clock and loses analysis rows to lock contention, so we
/// wait the scan out entirely — auto-analyze is best-effort background
/// work and a scan is always bounded by the library size. Stays
/// cancel-aware so a "Stop" click lands without waiting on the scan.
async fn wait_out_active_scan() {
    while crate::commands::scan::scan_in_flight() {
        if ANALYSIS_CANCEL.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(SCAN_WAIT_POLL).await;
    }
}

/// Signal the in-flight library analyzer to stop at the next track
/// boundary. Returns `true` when a run was actually in flight (so
/// the frontend can show a confirmation toast); `false` is a no-op
/// when nothing was running — clicking "Stop" twice or before
/// "Start" shouldn't surface as an error.
///
/// Idempotent: setting the cancel flag while it's already `true` is
/// fine; the worker loop clears it at the start of the next run.
#[tauri::command]
pub fn cancel_library_analysis() -> bool {
    let was_running = ANALYSIS_RUNNING.load(Ordering::Relaxed);
    if was_running {
        ANALYSIS_CANCEL.store(true, Ordering::SeqCst);
    }
    was_running
}

/// Read the per-profile auto-analyze flag. `true` when the user has
/// opted in to running the analyzer in the background after each
/// scan; defaults to `false` so the first scan stays fast and free.
#[tauri::command]
pub async fn get_auto_analyze(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    let pool = match state.require_profile_pool().await {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    Ok(read_auto_analyze(&pool).await)
}

/// Toggle the per-profile auto-analyze flag. Persisted in
/// `profile_setting` so it survives restarts; `false` removes the
/// row instead of writing `false` so the table stays sparse.
#[tauri::command]
pub async fn set_auto_analyze(state: tauri::State<'_, AppState>, enable: bool) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(AUTO_ANALYZE_KEY)
    .bind(if enable { "true" } else { "false" })
    .bind(now)
    .execute(&pool)
    .await?;
    Ok(())
}

async fn read_auto_analyze(pool: &SqlitePool) -> bool {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
        .bind(AUTO_ANALYZE_KEY)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    matches!(raw.as_deref(), Some("true"))
}

/// Spawn a background task that runs the full analyzer if the
/// auto-analyze flag is set for the active profile. Called from
/// scan callers after `summary.added > 0`. No-op when the flag is
/// off, when there's no active profile, or when the spawn itself
/// fails — auto-analyze is best-effort by definition.
pub fn maybe_auto_analyze(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let pool = match state.require_profile_pool().await {
            Ok(p) => p,
            Err(_) => return,
        };
        if !read_auto_analyze(&pool).await {
            return;
        }
        if let Err(err) = run_analyze_library(&app, &pool).await {
            tracing::warn!(%err, "auto analyze run failed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::code_is_busy;

    #[test]
    fn busy_and_locked_primary_codes_match() {
        // Primary BUSY / LOCKED.
        assert!(code_is_busy("5"));
        assert!(code_is_busy("6"));
        // Extended flavours fold to the same primary in the low byte:
        // BUSY_SNAPSHOT = 517 (5 | 2<<8), BUSY_RECOVERY = 261,
        // LOCKED_SHAREDCACHE = 262.
        assert!(code_is_busy("517"));
        assert!(code_is_busy("261"));
        assert!(code_is_busy("262"));
    }

    #[test]
    fn non_contention_codes_do_not_match() {
        // CONSTRAINT = 19, READONLY = 8, CANTOPEN = 14, OK = 0.
        assert!(!code_is_busy("19"));
        assert!(!code_is_busy("8"));
        assert!(!code_is_busy("14"));
        assert!(!code_is_busy("0"));
        // Garbage / non-numeric never counts as retryable.
        assert!(!code_is_busy(""));
        assert!(!code_is_busy("locked"));
    }
}
