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

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{
    analysis::{analyze_file, AnalysisResult},
    error::{AppError, AppResult},
    state::AppState,
};

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
    let path: Option<String> =
        sqlx::query_scalar("SELECT file_path FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&pool)
            .await?;
    let path = path.ok_or_else(|| {
        AppError::Other(format!("track {track_id} not found"))
    })?;
    let path_buf = PathBuf::from(path);

    // Decode is blocking + CPU-bound; keep it off the tokio reactor.
    let result: AnalysisResult = tokio::task::spawn_blocking(move || {
        analyze_file(&path_buf)
    })
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
    let pending: Vec<(i64, String)> = sqlx::query_as(
        "SELECT t.id, t.file_path
           FROM track t
           LEFT JOIN track_analysis ta ON ta.track_id = t.id
          WHERE t.is_available = 1 AND ta.track_id IS NULL
          ORDER BY t.id",
    )
    .fetch_all(&pool)
    .await?;

    let total = pending.len() as u32;
    let mut processed = 0u32;
    let mut failed = 0u32;

    for (track_id, file_path) in pending {
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
                let now = Utc::now().timestamp_millis();
                if let Err(e) = sqlx::query(
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
                .bind(track_id)
                .bind(result.bpm)
                .bind(result.loudness_db)
                .bind(result.replay_gain_db)
                .bind(result.peak)
                .bind(now)
                .execute(&pool)
                .await
                {
                    tracing::warn!(?e, track_id, "persist analysis failed");
                    failed += 1;
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
    }

    let summary = LibraryAnalysisSummary {
        processed,
        failed,
        // Currently nothing is skipped — the WHERE filter already
        // excludes already-analyzed rows. Reserved for a future
        // "skip if older than N days" option.
        skipped: 0,
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
