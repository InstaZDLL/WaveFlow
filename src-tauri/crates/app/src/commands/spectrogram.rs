//! The spectrogram Track Properties draws under the fake-lossless verdict,
//! so the user can see where the highs stop rather than take the verdict
//! on trust.
//!
//! The picture is recorded by the analysis' own bandwidth pass
//! (`waveflow_core::analysis::spectrogram_file`): same codec registry,
//! same frames, so the wall it shows is the one the verdict measured. It
//! costs a full decode of the file, so:
//!
//! - it runs on the blocking pool and announces itself to the task
//!   registry, with a progress measured in decoded samples against the
//!   length the container gives (indeterminate when it gives none);
//! - one at a time: opening the dialog on another track stops the decode
//!   in flight, and closing the dialog stops it too
//!   ([`cancel_track_spectrogram`]), as does the status bar's stop button;
//! - the last few pictures are kept in memory, keyed by the file's path,
//!   size and modification time, so reopening the dialog draws at once and
//!   a file replaced in between is decoded again.
//!
//! The cells travel as base64: a 900 × 256 picture is 230 KB of bytes,
//! which a JSON array of numbers would roughly triple.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use base64::Engine;
use serde::Serialize;
use tauri::AppHandle;
use waveflow_core::analysis::{spectrogram_file, SpectrogramError};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    tasks::{self, TaskKind},
};

/// Pictures kept in memory. Each is about 230 KB.
const CACHE_LEN: usize = 6;

/// Columns asked for when the frontend does not say. About the width of
/// the dialog's chart at 1.5× scale.
const DEFAULT_WIDTH: usize = 900;

/// The picture, ready to draw.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackSpectrogram {
    pub sample_rate: u32,
    pub columns: usize,
    pub rows: usize,
    pub seconds_per_column: f64,
    pub duration_seconds: f64,
    /// dB the byte scale spans under the loudest cell.
    pub range_db: f64,
    /// Column-major bytes, `rows` per column, row 0 = 0 Hz, base64.
    pub cells: String,
    /// The wall the same decode measured, in Hz; `None` for none.
    pub cutoff_hz: Option<f64>,
}

type CacheKey = (PathBuf, u64, Option<SystemTime>);

/// The decode in flight and the pictures already drawn.
#[derive(Default)]
pub struct SpectrogramJobs {
    current: Mutex<Option<Arc<AtomicBool>>>,
    cache: Mutex<VecDeque<(CacheKey, Arc<TrackSpectrogram>)>>,
}

impl SpectrogramJobs {
    /// Take the slot, stopping whatever held it.
    fn begin(&self) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = current.replace(flag.clone()) {
            previous.store(true, Ordering::Release);
        }
        flag
    }

    /// Give the slot back, unless a newer decode already took it.
    fn end(&self, flag: &Arc<AtomicBool>) {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, flag)) {
            *current = None;
        }
    }

    /// Take the slot for as long as the returned guard lives.
    fn slot(&self) -> Slot<'_> {
        Slot {
            jobs: self,
            flag: self.begin(),
        }
    }

    fn cancel(&self) {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flag) = current.as_ref() {
            flag.store(true, Ordering::Release);
        }
    }

    fn cached(&self, key: &CacheKey) -> Option<Arc<TrackSpectrogram>> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn remember(&self, key: CacheKey, picture: Arc<TrackSpectrogram>) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(k, _)| k.0 != key.0);
        cache.push_front((key, picture));
        cache.truncate(CACHE_LEN);
    }
}

/// The slot held by one request, given back on every way out of it.
struct Slot<'a> {
    jobs: &'a SpectrogramJobs,
    flag: Arc<AtomicBool>,
}

impl Slot<'_> {
    fn cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.jobs.end(&self.flag);
    }
}

/// Draw the spectrogram of a library track. `Ok(None)` when the decode
/// was stopped — by [`cancel_track_spectrogram`], by the dialog opening on
/// another track, or from the status bar.
#[tauri::command]
pub async fn track_spectrogram(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    jobs: tauri::State<'_, SpectrogramJobs>,
    track_id: i64,
    width: Option<usize>,
) -> AppResult<Option<Arc<TrackSpectrogram>>> {
    // Taken before the first await: a dialog closed while the database or
    // a sleeping drive answers must find something to cancel, or the
    // decode would start afterwards with nobody to show it to.
    let slot = jobs.slot();
    let pool = state.require_profile_pool().await?;
    let row: Option<(String, i64, Option<String>)> =
        sqlx::query_as("SELECT file_path, duration_ms, title FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
    drop(pool);
    let (path, duration_ms, title) =
        row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;
    let path = PathBuf::from(path);
    let meta = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || std::fs::metadata(path))
            .await
            .map_err(|e| AppError::Other(format!("spectrogram task panicked: {e}")))??
    };
    let key: CacheKey = (path.clone(), meta.len(), meta.modified().ok());
    if let Some(picture) = jobs.cached(&key) {
        return Ok(Some(picture));
    }
    if slot.cancelled() {
        return Ok(None);
    }

    let width = width.unwrap_or(DEFAULT_WIDTH).clamp(64, 2048);
    let flag = slot.flag.clone();
    let task = tasks::start(
        &app,
        TaskKind::Spectrogram,
        0,
        tasks::cancel_fn({
            let flag = flag.clone();
            move || flag.store(true, Ordering::Release)
        }),
    );
    if let (Some(task), Some(title)) = (task.as_ref(), title) {
        task.detail(title);
    }

    let decode_flag = flag.clone();
    let decode_path = path.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Progress in whole percent of the samples decoded: a packet is a
        // few thousand samples, the registry has no use for thousands of
        // updates a second, and the status bar prints the two numbers, which
        // read better as "40 / 100" than as sample counts. With no length
        // to divide by, the row stays indeterminate.
        let mut last_percent = None;
        spectrogram_file(
            &decode_path,
            width,
            |rate| {
                u64::try_from(duration_ms)
                    .ok()
                    .map(|ms| ms * u64::from(rate) / 1000)
            },
            |done, total| {
                if let Some(task) = task.as_ref().filter(|_| total > 0) {
                    // 99 at most while decoding: a declared length can be
                    // short, and a full bar would be a promise the decode
                    // has not kept yet. The row retires when it ends.
                    let percent = (done.min(total) * 100 / total).min(99);
                    if last_percent != Some(percent) {
                        task.progress(percent, 100);
                        last_percent = Some(percent);
                    }
                }
                !decode_flag.load(Ordering::Acquire)
            },
        )
    })
    .await
    .map_err(|e| AppError::Other(format!("spectrogram task panicked: {e}")));
    drop(slot);

    let (picture, bandwidth) = match result? {
        Ok(pair) => pair,
        Err(SpectrogramError::Cancelled) => return Ok(None),
        Err(SpectrogramError::Failed(message)) => return Err(AppError::Other(message)),
    };
    let picture = Arc::new(TrackSpectrogram {
        sample_rate: picture.sample_rate,
        columns: picture.columns,
        rows: picture.rows,
        seconds_per_column: picture.seconds_per_column,
        duration_seconds: picture.duration_seconds,
        range_db: waveflow_core::analysis::spectrogram::RANGE_DB,
        cells: base64::engine::general_purpose::STANDARD.encode(&picture.cells),
        cutoff_hz: bandwidth.and_then(|b| b.cutoff_hz),
    });
    jobs.remember(key, picture.clone());
    Ok(Some(picture))
}

/// Stop the spectrogram decode in flight, if any. The dialog calls it on
/// close; a decode that already finished is not affected.
#[tauri::command]
pub fn cancel_track_spectrogram(jobs: tauri::State<'_, SpectrogramJobs>) {
    jobs.cancel();
}

#[cfg(test)]
mod tests;
