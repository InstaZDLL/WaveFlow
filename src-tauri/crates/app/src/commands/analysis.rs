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
//! - `analyze_library`: iterates over every available track whose
//!   measurements are missing *or* were produced by a pass older than
//!   [`ANALYSIS_VERSION`], emitting progress events the UI can wire to
//!   a progress bar.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    analysis::{analyze_file, AnalysisResult, ANALYSIS_VERSION},
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
/// The analysis [`Slot`] guards against double-start. The user is the
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
static ANALYSIS_CANCEL: AtomicBool = AtomicBool::new(false);

/// The one sweep slot, and whether an automatic run was asked for while
/// it was taken.
///
/// The double-start guard used to drop such a request on the floor,
/// which loses work whenever the running sweep cannot cover it: tracks
/// a second scan adds after the sweep built its queue, or the profile a
/// switch opens while the previous profile's sweep is still draining.
/// [`maybe_auto_analyze`] now records it, and [`RunningGuard`] honours
/// it once when the current run ends — unless the user stopped that
/// run, since a stop is not a request to start over.
///
/// One mutex rather than two atomics: taking the slot, recording a
/// rerun and releasing the slot each have to see both fields at once,
/// or a request can land between the release and the read and be
/// consumed by the sweep that ended instead of the one it was meant
/// for. The lock is only ever held for those few lines, never across
/// an `.await` or a call out.
struct Slot {
    running: bool,
    rerun: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    running: false,
    rerun: false,
});

fn slot() -> std::sync::MutexGuard<'static, Slot> {
    // A panic while holding it cannot leave the two booleans
    // inconsistent, so a poisoned lock is still a usable one.
    SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Take the slot if it is free. When it is not and `queue` is set,
/// record that a run is wanted once it frees up.
fn reserve(queue: bool) -> bool {
    let mut slot = slot();
    if slot.running {
        if queue {
            slot.rerun = true;
        }
        return false;
    }
    // The run starting now covers whatever was waiting, and a stop
    // asked of the previous run does not carry over to this one.
    slot.running = true;
    slot.rerun = false;
    ANALYSIS_CANCEL.store(false, Ordering::SeqCst);
    true
}

/// Ask the running sweep to stop, under the slot's lock so a stop and
/// the end of a run cannot interleave: either the stop lands first and
/// the ending run sees it (no rerun), or the run has already released
/// the slot and there is nothing to stop. `false` when nothing ran.
fn request_stop() -> bool {
    let slot = slot();
    if slot.running {
        ANALYSIS_CANCEL.store(true, Ordering::SeqCst);
    }
    slot.running
}

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
    /// 0 to 1; see `waveflow_core::analysis::rhythm`. `None` on a row
    /// measured before analysis version 2.
    pub bpm_confidence: Option<f64>,
    /// 0 (calm) to 1 (intense), independent of the track's level.
    pub energy: Option<f64>,
    /// The key read from the audio (`Am`, `Eb`…), not the tag's: the
    /// properties dialog shows both.
    pub musical_key: Option<String>,
    /// 0 to 1; see `waveflow_core::analysis::key`.
    pub key_confidence: Option<f64>,
    pub loudness_lufs: Option<f64>,
    pub replay_gain_db: Option<f64>,
    pub peak: Option<f64>,
    /// Where the high frequencies stop, in hertz; `None` when they run
    /// to the top or were never measured (see `bandwidth_measured`).
    pub cutoff_hz: Option<f64>,
    /// Whether the cutoff was measured at all: rows analysed before it
    /// existed have none, and "no wall" must not read as "full band".
    pub bandwidth_measured: bool,
    /// A lossless file whose highs stop at a lossy encoder's wall —
    /// `waveflow_core::probable_fake_lossless_sql!`.
    pub probable_fake_lossless: bool,
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
    /// when the analysis slot was already taken on entry, since the
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
    read_analysis(&pool, track_id).await
}

/// One track's stored analysis, as the Properties dialog reads it. The
/// verdict on a fake lossless file needs the track's format as well as
/// the measure, hence the join.
async fn read_analysis(pool: &SqlitePool, track_id: i64) -> AppResult<Option<TrackAnalysisRow>> {
    #[derive(sqlx::FromRow)]
    struct Raw {
        track_id: i64,
        bpm: Option<f64>,
        bpm_confidence: Option<f64>,
        energy: Option<f64>,
        musical_key: Option<String>,
        key_confidence: Option<f64>,
        loudness_lufs: Option<f64>,
        replay_gain_db: Option<f64>,
        peak: Option<f64>,
        cutoff_hz: Option<f64>,
        bandwidth_version: Option<i64>,
        fake: bool,
        analyzed_at: i64,
    }
    let row: Option<Raw> = sqlx::query_as(concat!(
        "SELECT ta.track_id, ta.bpm, ta.bpm_confidence, ta.energy, ta.musical_key,
                ta.key_confidence, ta.loudness_lufs, ta.replay_gain_db, ta.peak,
                ta.cutoff_hz, ta.bandwidth_version,
                ",
        waveflow_core::probable_fake_lossless_sql!(),
        " AS fake,
                ta.analyzed_at
           FROM track_analysis ta
           JOIN track t ON t.id = ta.track_id
          WHERE ta.track_id = ?"
    ))
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| TrackAnalysisRow {
        track_id: r.track_id,
        bpm: r.bpm,
        bpm_confidence: r.bpm_confidence,
        energy: r.energy,
        musical_key: r.musical_key,
        key_confidence: r.key_confidence,
        loudness_lufs: r.loudness_lufs,
        replay_gain_db: r.replay_gain_db,
        peak: r.peak,
        cutoff_hz: r.cutoff_hz,
        bandwidth_measured: r.bandwidth_version.is_some(),
        probable_fake_lossless: r.fake,
        analyzed_at: r.analyzed_at,
    }))
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
        .fetch_optional(&*pool)
        .await?;
    let path = path.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;
    let path_buf = PathBuf::from(path);

    // Decode is blocking + CPU-bound; keep it off the tokio reactor.
    let result: AnalysisResult = tokio::task::spawn_blocking(move || analyze_file(&path_buf))
        .await
        .map_err(|e| AppError::Other(format!("analysis task panicked: {e}")))?
        .map_err(AppError::Other)?;

    let now = Utc::now().timestamp_millis();
    {
        let mut conn = pool.acquire().await?;
        upsert_analysis(&mut conn, track_id, &result, now).await?;
    }

    // Read back rather than built here: the fake-lossless verdict needs
    // the track's format, which lives on `track`.
    read_analysis(&pool, track_id)
        .await?
        .ok_or_else(|| AppError::Other(format!("track {track_id} lost its analysis row")))
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
    // Double-start guard: take the slot, or bail with a cancelled
    // summary so the caller's progress UI doesn't get stuck spinning.
    if !reserve(false) {
        tracing::info!("analyze_library called while another run is in flight; ignoring");
        return Ok(LibraryAnalysisSummary {
            processed: 0,
            failed: 0,
            skipped: 0,
            cancelled: true,
        });
    }
    run_reserved(app, pool).await
}

/// The sweep itself, for a caller that already holds the slot.
async fn run_reserved(app: &AppHandle, pool: &SqlitePool) -> AppResult<LibraryAnalysisSummary> {
    // The cancel flag was reset when the slot was taken (`reserve`): a
    // stale `true` from a prior stop would otherwise short-circuit this
    // run before its first track. The RAII guard at the end releases the
    // slot on every exit path (early returns + panics) so it can't stay
    // taken.
    let _guard = RunningGuard { app: app.clone() };

    // Announced before the query, with an unknown total. That query is
    // a join over every track in the library, which on a large one
    // takes long enough that a status bar staying empty through it
    // reads as nothing having started (#601). The sweep already had a
    // stopping mechanism and a progress event of its own; the registry
    // routes to the first and mirrors the second. Dropped at the end of
    // the function, on every exit path.
    let task = crate::tasks::start(
        app,
        crate::tasks::TaskKind::Analysis,
        0,
        crate::tasks::cancel_fn(|| {
            request_stop();
        }),
    );

    // Never measured, or measured by a pass older than the current one.
    // A versionless row is the case this was built for: its peak came
    // from a mono downmix, and until it is re-measured clipping
    // prevention has to refuse the boost that peak appears to allow.
    // Re-measuring is the only thing that lifts that restriction, so
    // the sweep has to pick those rows up — but it still never
    // *deletes* one, which was the decision taken when #545 shipped.
    //
    // Strictly older, not merely different: a profile carried back from
    // a build ahead of this one holds measurements we have no better
    // replacement for, and re-running an older analyzer over them would
    // downgrade the data. Playback stays safe in that case anyway —
    // `fetch_replay_gain` distrusts any version it doesn't recognise.
    // NULL needs its own arm because `NULL < ?` is NULL, not true.
    let pending: Vec<(i64, String)> = sqlx::query_as(concat!(
        "SELECT t.id, t.file_path
           FROM track t
           LEFT JOIN track_analysis ta ON ta.track_id = t.id
          WHERE t.is_available = 1
            AND (ta.track_id IS NULL
                 OR ta.analysis_version IS NULL
                 OR ta.analysis_version < ?
                 -- A newer cutoff measure re-runs the lossless files
                 -- alone: it is the only verdict that concerns them.
                 OR (",
        waveflow_core::lossless_track_sql!(),
        "
                     AND (ta.bandwidth_version IS NULL OR ta.bandwidth_version < ?)))
          ORDER BY t.id",
    ))
    .bind(ANALYSIS_VERSION)
    .bind(waveflow_core::analysis::bandwidth::BANDWIDTH_VERSION)
    .fetch_all(pool)
    .await?;

    let total = pending.len() as u32;
    if let Some(task) = task.as_ref() {
        // The count is only knowable now, so the row that has been
        // sitting there indeterminate gets its total here.
        task.progress(0, total as u64);
    }
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
        // After the increment, so the last track moves the bar to full
        // rather than leaving it one short for the instant before the
        // row retires. `TaskHandle::progress` does not throttle the
        // tick that completes a task, which is what makes that visible.
        if let Some(task) = task.as_ref() {
            task.progress(processed as u64, total as u64);
        }

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

/// RAII guard that releases the analysis [`Slot`] on drop. Lets every
/// exit path of `run_analyze_library` — early return on cancel, the
/// SQL error path, a future `?` on a new query, even a panic during
/// decode — reset the running flag without explicit cleanup. Without
/// this a single error from `sqlx::query_as` would leave the static
/// `true` forever and brick auto-analyze for the rest of the
/// session.
///
/// It is also where a request refused mid-run ([`Slot::rerun`]) is
/// picked up: every way a run ends passes through here, so this is the
/// one place where "the sweep that refused it is over" is certain.
struct RunningGuard {
    app: AppHandle,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let rerun = {
            let mut slot = slot();
            // Under the lock, like `request_stop` and `reserve`: a stop
            // cannot slip in between this read and the release.
            let stopped_by_user = ANALYSIS_CANCEL.load(Ordering::SeqCst);
            let wanted = slot.rerun && !stopped_by_user;
            slot.running = false;
            slot.rerun = false;
            wanted
        };
        if rerun {
            // Re-resolves the active profile and its toggle: after a
            // profile switch that is the profile the request was for.
            maybe_auto_analyze(&self.app);
        }
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
        upsert_analysis(&mut tx, p.track_id, &p.result, p.analyzed_at).await?;
    }
    tx.commit().await
}

/// Write one track's measurements, stamped with [`ANALYSIS_VERSION`].
/// The single writer of `track_analysis` rows, for the one-track command
/// and the library sweep alike, so a column added to the analysis cannot
/// reach one of them and not the other. `musical_key` here is the key
/// read from the audio; the tag's lives on `track` and wins when it is
/// there (`waveflow_core::effective_musical_key_sql!`).
async fn upsert_analysis(
    conn: &mut sqlx::SqliteConnection,
    track_id: i64,
    result: &AnalysisResult,
    analyzed_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO track_analysis
            (track_id, bpm, bpm_confidence, energy, musical_key, key_confidence,
             beats, cutoff_hz, bandwidth_version,
             loudness_lufs, replay_gain_db, peak, analyzed_at, analysis_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            bpm              = excluded.bpm,
            bpm_confidence   = excluded.bpm_confidence,
            energy           = excluded.energy,
            musical_key      = excluded.musical_key,
            key_confidence   = excluded.key_confidence,
            beats            = excluded.beats,
            cutoff_hz        = excluded.cutoff_hz,
            bandwidth_version = excluded.bandwidth_version,
            loudness_lufs    = excluded.loudness_lufs,
            replay_gain_db   = excluded.replay_gain_db,
            peak             = excluded.peak,
            analyzed_at      = excluded.analyzed_at,
            analysis_version = excluded.analysis_version",
    )
    .bind(track_id)
    .bind(result.bpm)
    .bind(result.bpm_confidence)
    .bind(result.energy)
    .bind(result.key.map(|k| k.name()))
    .bind(result.key.map(|k| k.confidence))
    .bind(waveflow_core::analysis::beats::encode(&result.beats))
    .bind(result.bandwidth.and_then(|b| b.cutoff_hz))
    // Stamped whenever this pass ran, measure or not: a file the meter
    // found nothing in (silence) must not be swept again forever.
    .bind(waveflow_core::analysis::bandwidth::BANDWIDTH_VERSION)
    .bind(result.loudness_lufs)
    .bind(result.replay_gain_db)
    .bind(result.peak)
    .bind(analyzed_at)
    .bind(ANALYSIS_VERSION)
    .execute(conn)
    .await?;
    Ok(())
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
        // Park the write behind any in-flight scan before EVERY
        // attempt — not just the decode that fed this batch. A scan
        // can start during that track's ~8 s decode, during a prior
        // retry's busy-wait, or the batch can be the gate-less
        // end-of-run flush. Without re-parking here the 6-attempt
        // budget could be burned spinning against a still-active scan
        // and the batch dropped, reintroducing the data-loss bug.
        // Cancel-aware (via `wait_out_active_scan`): a "Stop" lets the
        // attempt proceed so the decoded rows still land.
        wait_out_active_scan().await;
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
/// fine; taking the slot for the next run clears it.
#[tauri::command]
pub fn cancel_library_analysis() -> bool {
    request_stop()
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
    .execute(&*pool)
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
/// scan callers after `summary.added > 0`, and when a profile opens (at
/// startup, after the startup scan if there is one, and on a profile
/// switch) so rows from an older `ANALYSIS_VERSION` are re-measured
/// without waiting for a new file. No-op when the flag is
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
        // Take the slot, or leave the request for the sweep holding it
        // to pick up when it ends.
        if !reserve(true) {
            return;
        }
        if let Err(err) = run_reserved(&app, &pool).await {
            tracing::warn!(%err, "auto analyze run failed");
        }
    });
}

#[cfg(test)]
mod tests;
