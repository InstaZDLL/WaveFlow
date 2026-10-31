//! The tag workshop's commands: open a set of files, preview what a
//! change would rewrite, write it as a journaled batch, undo a batch,
//! and propose tags from a MusicBrainz release.
//!
//! The model, the diff and the journal are `waveflow_core::tag_workshop`;
//! the match is the enrichment engine's. What is here is the part that
//! needs the app: the files, the audio engine, the task registry.
//!
//! # Writing a batch
//!
//! One file at a time, and for each, in this order:
//!
//! 1. wait out a running scan — the workshop is a second writer, and it
//!    owes the scan the database (`docs/architecture/invariants.md`,
//!    "Single writer to SQLite");
//! 2. read the file's tags **now**, and refuse it if they are no longer
//!    the ones the preview showed: nothing is written that was not seen;
//! 3. journal the old and new values (status `pending`);
//! 4. pause playback if this is the track playing, then write through
//!    `edit::patch_file` — tags only, never the audio;
//! 5. re-hash the whole file into `track.file_hash`, and keep the same
//!    hash in the journal: it is what an undo checks the file against;
//! 6. bring the database in line through `edit::sync_db` — the same
//!    relink of artists, album and genres the properties dialog runs.
//!
//! The batch stops between two files when the task is cancelled, and
//! every database write is a short transaction retried on a busy lock.
//!
//! # Undoing a batch
//!
//! Each written file whose bytes still hash to what the batch left gets
//! its old values back, the same way. One that changed since — another
//! tagger, a later batch — is refused and reported, never overwritten.

mod read;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use waveflow_core::enrichment::resolve;
use waveflow_core::enrichment::{LocalAlbum, LocalTrack};
use waveflow_core::error::CoreError;
use waveflow_core::metadata::album_match::TrackSignals;
use waveflow_core::tag_workshop::proposal::{self, AlbumProposal};
use waveflow_core::tag_workshop::{diff, journal, Field, FieldDiff, Values};

use crate::audio::AudioEngine;
use crate::commands::edit::{patch_file, rehash_track_file, sync_db, TagPatch};
use crate::commands::enrichment::{self, ProfileDb};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub(crate) use read::read_values;
use read::{first_invalid, to_db_edit, to_edit};

/// Emitted when a batch is written or undone: the list of batches
/// re-reads itself.
pub const CHANGED_EVENT: &str = "tag-workshop:changed";

/// A file's problem when the file is as asked but its row is not: the
/// database update after the write failed. The next scan of the folder
/// reads the file back (see `tag_workshop_undo`).
const DATABASE_STALE: &str = "database_stale";

/// Files opened at once, at most. A table of this many rows is still
/// one page scroller away; past it, the user is better served by a
/// narrower selection than by a minute of reading tags.
const MAX_FILES: usize = 5000;
/// Batches the list returns.
const BATCH_LIST_LIMIT: i64 = 200;
const SCAN_WAIT_POLL: Duration = Duration::from_millis(250);

/// One batch writes or undoes at a time: two would interleave writes to
/// the same files, and their journals would disagree about what was there.
static BUSY: AtomicBool = AtomicBool::new(false);

struct BusyGuard;

impl BusyGuard {
    fn take() -> AppResult<Self> {
        if BUSY.swap(true, Ordering::SeqCst) {
            return Err(AppError::Other(
                "the tag workshop is already writing a batch".into(),
            ));
        }
        Ok(BusyGuard)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn is_busy(err: &AppError) -> bool {
    match err {
        AppError::Database(e) | AppError::Core(CoreError::Database(e)) => {
            crate::commands::analysis::is_busy(e)
        }
        _ => false,
    }
}

/// Run `attempt` again while the database answers busy, as the analysis
/// sweep and the enrichment engine do. For the writes that manage their
/// own transaction (`sync_db`, the re-hash).
async fn retry_busy<T, F, Fut>(mut attempt: F) -> AppResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    const MAX_ATTEMPTS: usize = 6;
    let mut backoff = Duration::from_millis(50);
    let mut tries = 0;
    loop {
        tries += 1;
        match attempt().await {
            Err(err) if is_busy(&err) && tries < MAX_ATTEMPTS => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(2));
            }
            other => return other,
        }
    }
}

/// Park while a scan walks and writes. `false` when the batch was
/// cancelled meanwhile.
async fn wait_out_scan(task: Option<&crate::tasks::TaskHandle>) -> bool {
    while crate::commands::scan::scan_in_flight() {
        if task.is_some_and(|t| t.is_cancelling()) {
            return false;
        }
        tokio::time::sleep(SCAN_WAIT_POLL).await;
    }
    !task.is_some_and(|t| t.is_cancelling())
}

/// Pause the engine if `track_id` is the track it plays: on Windows its
/// read handle refuses the write. Resume is the user's, as in the
/// properties dialog.
async fn pause_if_playing(engine: &AudioEngine, track_id: Option<i64>) {
    let Some(track_id) = track_id else {
        return;
    };
    let active = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    if active > 0 && active == track_id {
        let _ = engine.send(crate::audio::AudioCmd::Pause);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn read_blocking(path: PathBuf) -> Result<Values, String> {
    tokio::task::spawn_blocking(move || read_values(&path))
        .await
        .map_err(|e| format!("read join: {e}"))?
}

async fn full_hash(path: PathBuf) -> std::io::Result<String> {
    tokio::task::spawn_blocking(move || waveflow_core::scanner::extract::hash_file_full(&path))
        .await
        .map_err(std::io::Error::other)?
}

// ---------------------------------------------------------------------
// Opening files
// ---------------------------------------------------------------------

/// What the workshop opens.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkshopSource {
    /// A selection, a folder's files: in the order given.
    Tracks { track_ids: Vec<i64> },
    /// An album's available tracks, in disc then track order.
    Album { album_id: i64 },
}

/// One file in the workshop's table.
#[derive(Debug, Serialize)]
pub struct WorkshopRow {
    pub track_id: i64,
    pub album_id: Option<i64>,
    pub file_path: String,
    pub duration_ms: i64,
    /// The file's tags as it carries them now.
    pub values: Values,
    /// Why the file cannot be edited: unreadable, or a container no tag
    /// can be written into. Such a row is shown and never written.
    pub error: Option<String>,
}

type TrackRow = (i64, Option<i64>, String, i64);

/// What the workshop opened.
#[derive(Debug, Serialize)]
pub struct WorkshopOpened {
    pub rows: Vec<WorkshopRow>,
    /// How many files the source holds: more than `rows` when it was cut
    /// to [`MAX_FILES`], which the view says.
    pub total: usize,
}

/// Every command takes the profile the workshop was opened in and is
/// refused once another one is active: a table, a batch number or a
/// track id means nothing in another profile's database, and an undo sent
/// there would rewrite that profile's files.
async fn profile_pool(state: &AppState, profile_id: i64) -> AppResult<crate::state::ProfilePool> {
    state.require_profile_pool_for(Some(profile_id)).await
}

#[tauri::command]
pub async fn tag_workshop_open(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
    source: WorkshopSource,
) -> AppResult<WorkshopOpened> {
    let pool = profile_pool(&state, profile_id).await?;
    let rows: Vec<TrackRow> = match source {
        WorkshopSource::Album { album_id } => {
            sqlx::query_as(
                "SELECT id, album_id, file_path, duration_ms
                   FROM track
                  WHERE album_id = ? AND is_available = 1
                  ORDER BY COALESCE(disc_number, 1),
                           COALESCE(track_number, 2147483647),
                           file_path",
            )
            .bind(album_id)
            .fetch_all(&*pool)
            .await?
        }
        WorkshopSource::Tracks { track_ids } => {
            let mut out = Vec::with_capacity(track_ids.len().min(MAX_FILES));
            let mut seen = std::collections::HashSet::new();
            for id in track_ids {
                if !seen.insert(id) {
                    continue;
                }
                // Remote tracks have no file to tag: `file_path` is not
                // a path on this machine for them, and a server-only row
                // is not available.
                let row: Option<TrackRow> = sqlx::query_as(
                    "SELECT id, album_id, file_path, duration_ms
                       FROM track
                      WHERE id = ? AND is_available = 1",
                )
                .bind(id)
                .fetch_optional(&*pool)
                .await?;
                out.extend(row);
            }
            out
        }
    };
    drop(pool);
    let total = rows.len();
    let rows: Vec<TrackRow> = rows.into_iter().take(MAX_FILES).collect();
    let out = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|(track_id, album_id, file_path, duration_ms)| {
                let read = read_values(Path::new(&file_path));
                let (values, error) = match read {
                    Ok(values) => (values, None),
                    Err(err) => (Values::new(), Some(err)),
                };
                WorkshopRow {
                    track_id,
                    album_id,
                    file_path,
                    duration_ms,
                    values,
                    error,
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| AppError::Other(format!("tag workshop read join: {e}")))?;
    Ok(WorkshopOpened { rows: out, total })
}

// ---------------------------------------------------------------------
// Previewing
// ---------------------------------------------------------------------

/// The values asked of one file, for a preview.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackChange {
    pub track_id: i64,
    pub values: Values,
}

/// The values a write puts into one file, and `expected`: what the
/// preview showed as each changed field's old value. Required, never
/// defaulted — the write refuses a file whose tags moved since, so what
/// is written is what was seen, and a write without it would skip that.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackWrite {
    pub track_id: i64,
    pub values: Values,
    pub expected: Values,
}

/// A value that cannot be written, and why (a key the frontend
/// translates).
#[derive(Debug, Clone, Serialize)]
pub struct InvalidValue {
    pub field: Field,
    pub reason: &'static str,
}

/// What a change would do to one file.
#[derive(Debug, Serialize)]
pub struct FilePreview {
    pub track_id: i64,
    pub file_path: String,
    pub diffs: Vec<FieldDiff>,
    pub invalid: Option<InvalidValue>,
    pub error: Option<String>,
}

async fn track_path(pool: &sqlx::SqlitePool, track_id: i64) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT file_path FROM track WHERE id = ? AND is_available = 1")
            .bind(track_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// The files `changes` would rewrite and, for each, the fields: what the
/// file says now beside what it would say. Files the change leaves as
/// they are do not appear. Nothing is written.
#[tauri::command]
pub async fn tag_workshop_preview(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
    changes: Vec<TrackChange>,
) -> AppResult<Vec<FilePreview>> {
    let pool = profile_pool(&state, profile_id).await?;
    let mut paths = Vec::with_capacity(changes.len());
    for change in &changes {
        paths.push(track_path(&pool, change.track_id).await?);
    }
    drop(pool);
    let out = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        for (change, path) in changes.into_iter().zip(paths) {
            let Some(path) = path else {
                out.push(FilePreview {
                    track_id: change.track_id,
                    file_path: String::new(),
                    diffs: Vec::new(),
                    invalid: None,
                    error: Some("track not found".into()),
                });
                continue;
            };
            let invalid =
                first_invalid(&change.values).map(|(field, reason)| InvalidValue { field, reason });
            let (diffs, error) = match read_values(Path::new(&path)) {
                Ok(current) => (diff(&current, &change.values), None),
                Err(err) => (Vec::new(), Some(err)),
            };
            if diffs.is_empty() && invalid.is_none() && error.is_none() {
                continue;
            }
            out.push(FilePreview {
                track_id: change.track_id,
                file_path: path,
                diffs,
                invalid,
                error,
            });
        }
        out
    })
    .await
    .map_err(|e| AppError::Other(format!("tag workshop preview join: {e}")))?;
    Ok(out)
}

// ---------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------

/// The cover a matched release offers, filled into each album the batch
/// leaves without one — what the enrichment engine does.
#[derive(Debug, Clone, Deserialize)]
pub struct CoverRequest {
    pub release_mbid: String,
    pub release_group_mbid: Option<String>,
    pub has_front_cover: bool,
}

/// One file a batch did not write, and why.
#[derive(Debug, Clone, Serialize)]
pub struct FileProblem {
    pub track_id: Option<i64>,
    pub file_path: String,
    /// `changed_since` (the file moved since it was previewed or written),
    /// `missing`, `interrupted`, `invalid`, or the write's own message.
    pub reason: String,
}

#[derive(Debug, Default, Serialize)]
pub struct ApplyReport {
    /// `None` when nothing was written, in which case no batch is kept.
    pub batch_id: Option<i64>,
    pub written: u32,
    pub unchanged: u32,
    /// The tracks whose files now say what was asked — written, or found
    /// already so. The table drops their edits and keeps every other's:
    /// a refused file, and the ones a stopped batch never reached.
    pub done: Vec<i64>,
    pub problems: Vec<FileProblem>,
    pub cancelled: bool,
    pub covers: u32,
}

/// Write `changes` as one batch. See the module documentation for the
/// order of a file's write and why.
#[tauri::command]
pub async fn tag_workshop_apply(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    profile_id: i64,
    changes: Vec<TrackWrite>,
    label: String,
    release_mbid: Option<String>,
    cover: Option<CoverRequest>,
) -> AppResult<ApplyReport> {
    let _busy = BusyGuard::take()?;
    // A lease per file rather than one for the batch, as the enrichment
    // engine holds it: a profile switch waits for one file, and a step
    // can never land in the next profile's database.
    drop(profile_pool(&state, profile_id).await?);
    let db = ProfileDb::for_command(&app, profile_id);
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let release_mbid = release_mbid
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| !m.is_empty());

    let total = changes.len() as u64;
    let task = crate::tasks::start(
        &app,
        crate::tasks::TaskKind::TagWorkshop,
        total,
        crate::tasks::Cancellation::Flag,
    );
    if let Some(task) = &task {
        task.detail(label.clone());
    }

    let now = now_ms();
    let batch_id = {
        let label = label.clone();
        let release = release_mbid.clone();
        db.write(move |conn| {
            let label = label.clone();
            let release = release.clone();
            Box::pin(
                async move { journal::create_batch(conn, &label, release.as_deref(), now).await },
            )
        })
        .await?
    };

    let mut report = ApplyReport::default();
    // The profile was switched under the batch: its database is closing,
    // and nothing after the loop can reach it.
    let mut profile_gone = false;
    // The albums the written files that now name `release_mbid` are in:
    // the only ones whose enrichment decision the files can settle, and
    // the only ones that release's cover belongs to.
    let mut albums_naming_release: Vec<i64> = Vec::new();

    for (index, change) in changes.into_iter().enumerate() {
        if let Some(task) = &task {
            task.progress(index as u64, total);
        }
        if !wait_out_scan(task.as_ref()).await {
            report.cancelled = true;
            break;
        }
        // One lease for the whole of this file, from the journal entry to
        // its confirmation: a profile switch waits for it rather than
        // closing the database between the write and its record. Taken
        // anew per file, so the switch waits for one file at most; refused
        // once the profile is no longer the one the batch started on.
        let Ok(pool) = db.lease().await else {
            report.cancelled = true;
            profile_gone = true;
            break;
        };
        // A database that fails here fails every file after: the batch
        // stops, and what it did so far is settled and reported below
        // rather than lost with an error.
        let path = match track_path(&pool, change.track_id).await {
            Ok(path) => path,
            Err(err) => {
                report.problems.push(FileProblem {
                    track_id: Some(change.track_id),
                    file_path: String::new(),
                    reason: format!("database error: {err}"),
                });
                report.cancelled = true;
                break;
            }
        };
        let Some(path) = path else {
            report.problems.push(FileProblem {
                track_id: Some(change.track_id),
                file_path: String::new(),
                reason: "missing".into(),
            });
            continue;
        };
        let problem = |reason: String| FileProblem {
            track_id: Some(change.track_id),
            file_path: path.clone(),
            reason,
        };
        if first_invalid(&change.values).is_some() {
            report.problems.push(problem("invalid".into()));
            continue;
        }

        // 2. The file as it is now.
        let current = match read_blocking(PathBuf::from(&path)).await {
            Ok(values) => values,
            Err(err) => {
                report.problems.push(problem(err));
                continue;
            }
        };
        let diffs = diff(&current, &change.values);
        let seen: Values = diffs.iter().map(|d| (d.field, d.old.clone())).collect();
        let expected: Values = change
            .expected
            .iter()
            .map(|(f, v)| (*f, waveflow_core::tag_workshop::normalize(*f, v)))
            .collect();
        if seen != expected {
            report.problems.push(problem("changed_since".into()));
            continue;
        }
        if diffs.is_empty() {
            report.unchanged += 1;
            report.done.push(change.track_id);
            continue;
        }
        let written: Values = diffs.iter().map(|d| (d.field, d.new.clone())).collect();

        // 3. Journaled before the write. Not journaled, not written: the
        //    file is left as it is and the batch stops.
        let journaled = {
            let path = path.clone();
            let diffs = diffs.clone();
            let track_id = change.track_id;
            enrichment::write_now(&pool, move |conn| {
                let path = path.clone();
                let diffs = diffs.clone();
                Box::pin(async move {
                    journal::journal_file(conn, batch_id, Some(track_id), &path, &diffs).await
                })
            })
            .await
        };
        let file_id = match journaled {
            Ok(file_id) => file_id,
            Err(err) => {
                report
                    .problems
                    .push(problem(format!("database error: {err}")));
                report.cancelled = true;
                break;
            }
        };

        // 4. The write.
        pause_if_playing(&engine, Some(change.track_id)).await;
        let file_edit = to_edit(&written);
        let outcome = {
            let path = PathBuf::from(&path);
            tokio::task::spawn_blocking(move || {
                patch_file(&path, &TagPatch::Fields(&file_edit)).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("tag write join: {e}"))
            .and_then(|r| r)
        };
        if let Err(err) = outcome {
            let message = format!("tag write failed: {err}");
            let failed = message.clone();
            let recorded = enrichment::write_now(&pool, move |conn| {
                let failed = failed.clone();
                Box::pin(async move { journal::mark_failed(conn, file_id, &failed).await })
            })
            .await;
            report.problems.push(problem(message));
            if let Err(err) = recorded {
                // The file is untouched, but its entry stays `pending`: an
                // undo refuses it as interrupted, which writes nothing —
                // the safe answer for a write nothing confirms either way.
                tracing::warn!(file_id, %err, "tag workshop could not record a refused write");
                report.cancelled = true;
                break;
            }
            continue;
        }

        // 5. The hash the write left, journaled at once: from here on the
        //    file is changed, and its record is what lets an undo take the
        //    change back — whatever happens to the database next.
        let hash = match retry_busy(|| rehash_track_file(&pool, change.track_id, Path::new(&path)))
            .await
        {
            Ok(hash) => Some(hash),
            Err(err) => {
                tracing::warn!(track_id = change.track_id, %err, "tag workshop re-hash failed");
                full_hash(PathBuf::from(&path)).await.ok()
            }
        };
        let confirmed = match &hash {
            Some(hash) => {
                let hash = hash.clone();
                enrichment::write_now(&pool, move |conn| {
                    let hash = hash.clone();
                    Box::pin(async move { journal::mark_written(conn, file_id, &hash).await })
                })
                .await
                .is_ok()
            }
            None => false,
        };
        report.written += 1;
        if !confirmed {
            // Written, but the record of it could not be completed: the
            // file stays `pending`, which an undo refuses rather than guess.
            report.problems.push(problem("interrupted".into()));
        }

        // 6. The database in line with the file. A failure here leaves the
        //    file written and journaled; the next scan of the folder reads
        //    it back, so it is reported and the batch goes on.
        let db_edit = to_db_edit(&written, Path::new(&path));
        if let Err(err) = retry_busy(|| sync_db(&pool, change.track_id, &db_edit)).await {
            tracing::warn!(track_id = change.track_id, %err, "tag workshop database update failed");
            report.problems.push(problem(DATABASE_STALE.into()));
            continue;
        }
        let album_after: Option<i64> =
            sqlx::query_scalar("SELECT album_id FROM track WHERE id = ?")
                .bind(change.track_id)
                .fetch_optional(&*pool)
                .await
                .ok()
                .flatten()
                .flatten();
        drop(pool);
        if confirmed {
            report.done.push(change.track_id);
        }
        // The release the file names now: the one just written, else the
        // one it already carried. Only the album the file is in now is
        // settled by it: the one it left keeps the decision its other
        // files earned.
        let named = written
            .get(&Field::MbReleaseId)
            .or_else(|| current.get(&Field::MbReleaseId));
        if named.is_some() && named.map(String::as_str) == release_mbid.as_deref() {
            albums_naming_release.extend(album_after);
        }
        let _ = app.emit("track:updated", change.track_id);
    }
    if let Some(task) = &task {
        task.progress(total, total);
    }

    if profile_gone {
        // What was written is journaled; the batch is settled, its
        // decisions reopened and its covers fetched the next time — or
        // never, which leaves the files and their journal as they are.
        // The partial report is what the user needs now.
        report.batch_id = (report.written > 0).then_some(batch_id);
        drop(task);
        let _ = app.emit(CHANGED_EVENT, ());
        return Ok(report);
    }
    // The finalisation reports, it does not fail: past this point files
    // may be written, and the report of them is what the user needs.
    let kept = match db
        .write(move |conn| Box::pin(async move { journal::discard_if_empty(conn, batch_id).await }))
        .await
    {
        Ok(discarded) => !discarded,
        Err(err) => {
            tracing::warn!(batch_id, %err, "tag workshop could not settle the batch");
            true
        }
    };
    report.batch_id = kept.then_some(batch_id);

    albums_naming_release.sort_unstable();
    albums_naming_release.dedup();
    if let Some(release) = release_mbid.as_deref() {
        if !albums_naming_release.is_empty() {
            if let Err(err) =
                reopen_enrichment_decisions(&app, &db, albums_naming_release.clone(), release).await
            {
                tracing::warn!(%err, "tag workshop could not reopen enrichment decisions");
            }
        }
    }
    // The cover of the release the files now name, for their albums only:
    // a genre fixed in another album of the same batch gives that album
    // nothing of this release.
    let cover = cover.filter(|c| release_mbid.as_deref() == Some(c.release_mbid.as_str()));
    if let Some(cover) = cover.filter(|_| !albums_naming_release.is_empty()) {
        if !crate::offline::is_offline() {
            let (_, caa) = enrichment::clients();
            for album_id in &albums_naming_release {
                match enrichment::fetch_cover(
                    &db,
                    &caa,
                    &artwork_dir,
                    *album_id,
                    &cover.release_mbid,
                    cover.release_group_mbid.as_deref(),
                    cover.has_front_cover,
                )
                .await
                {
                    Ok(true) => report.covers += 1,
                    Ok(false) => {}
                    Err(err) => tracing::warn!(album_id, %err, "tag workshop cover fetch failed"),
                }
            }
        }
    }

    drop(task);
    let _ = app.emit(CHANGED_EVENT, ());
    let _ = app.emit("library:rescanned", ());
    let _ = app.emit("player:queue-changed", ());
    if report.written > 0 {
        state.drain.notify();
    }
    Ok(report)
}

/// The files now name `release`: an enrichment decision about one of
/// these albums that says otherwise — a proposal under review, a
/// rejection, a match with another release — is forgotten, so the
/// engine's next pass reads the identifier from the files and settles
/// it. A decision for the very release is kept.
async fn reopen_enrichment_decisions(
    app: &AppHandle,
    db: &ProfileDb,
    albums: Vec<i64>,
    release: &str,
) -> AppResult<()> {
    let release = release.to_owned();
    let now = now_ms();
    let changed = db
        .write(move |conn| {
            let albums = albums.clone();
            let release = release.clone();
            Box::pin(async move {
                let mut changed = false;
                for album_id in albums {
                    let decision: Option<(String, Option<String>)> = sqlx::query_as(
                        "SELECT status, release_mbid FROM musicbrainz_album WHERE album_id = ?",
                    )
                    .bind(album_id)
                    .fetch_optional(&mut *conn)
                    .await?;
                    let Some((status, mbid)) = decision else {
                        continue;
                    };
                    if status == "applied" && mbid.as_deref() == Some(release.as_str()) {
                        continue;
                    }
                    waveflow_core::enrichment::store::forget_album(conn, album_id, now).await?;
                    changed = true;
                }
                Ok(changed)
            })
        })
        .await?;
    if changed {
        let _ = app.emit(enrichment::CHANGED_EVENT, ());
    }
    Ok(())
}

// ---------------------------------------------------------------------
// The journal
// ---------------------------------------------------------------------

#[tauri::command]
pub async fn tag_workshop_batches(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
) -> AppResult<Vec<journal::BatchSummary>> {
    let pool = profile_pool(&state, profile_id).await?;
    let mut conn = pool.acquire().await?;
    Ok(journal::batches(&mut conn, BATCH_LIST_LIMIT).await?)
}

#[tauri::command]
pub async fn tag_workshop_batch_files(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
    batch_id: i64,
) -> AppResult<Vec<journal::JournaledFile>> {
    let pool = profile_pool(&state, profile_id).await?;
    let mut conn = pool.acquire().await?;
    Ok(journal::batch_files(&mut conn, batch_id).await?)
}

#[derive(Debug, Default, Serialize)]
pub struct UndoReport {
    pub restored: u32,
    pub refused: Vec<FileProblem>,
    pub cancelled: bool,
    /// Every file of the batch is restored.
    pub done: bool,
}

/// Put back the values a batch replaced, in every file it wrote that has
/// not changed since. The others are reported and left as they are.
#[tauri::command]
pub async fn tag_workshop_undo(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    profile_id: i64,
    batch_id: i64,
) -> AppResult<UndoReport> {
    let _busy = BusyGuard::take()?;
    let pool = profile_pool(&state, profile_id).await?;
    let files = {
        let mut conn = pool.acquire().await?;
        journal::batch_files(&mut conn, batch_id).await?
    };
    drop(pool);
    let db = ProfileDb::for_command(&app, profile_id);
    let open: Vec<journal::JournaledFile> = files
        .into_iter()
        .filter(|f| {
            matches!(
                f.status,
                journal::FileStatus::Written | journal::FileStatus::Pending
            )
        })
        .collect();
    let total = open.len() as u64;
    let task = crate::tasks::start(
        &app,
        crate::tasks::TaskKind::TagWorkshop,
        total,
        crate::tasks::Cancellation::Flag,
    );
    let mut report = UndoReport::default();
    let mut profile_gone = false;

    for (index, file) in open.into_iter().enumerate() {
        if let Some(task) = &task {
            task.progress(index as u64, total);
        }
        if !wait_out_scan(task.as_ref()).await {
            report.cancelled = true;
            break;
        }
        // One lease for the whole of this file, as the write takes it.
        let Ok(pool) = db.lease().await else {
            report.cancelled = true;
            profile_gone = true;
            break;
        };
        let refuse = |reason: &str| FileProblem {
            track_id: file.track_id,
            file_path: file.file_path.clone(),
            reason: reason.to_owned(),
        };
        // A file journaled and never confirmed may or may not have been
        // written; nothing says what its bytes should be, so nothing
        // tells it apart from one changed since.
        let (journal::FileStatus::Written, Some(hash_after)) = (file.status, &file.hash_after)
        else {
            report.refused.push(refuse("interrupted"));
            continue;
        };
        let path = PathBuf::from(&file.file_path);
        match full_hash(path.clone()).await {
            Ok(hash) if &hash == hash_after => {}
            Ok(_) => {
                report.refused.push(refuse("changed_since"));
                continue;
            }
            Err(_) => {
                report.refused.push(refuse("missing"));
                continue;
            }
        }

        // The old values, an absent one as an empty value: removed.
        let restore: Values = file
            .fields
            .iter()
            .map(|f| (f.field, f.old.clone().unwrap_or_default()))
            .collect();
        // The track that is this file now, if it still is.
        let track_id = match file.track_id {
            Some(id) => match track_path(&pool, id).await {
                Ok(current) => (current.as_deref() == Some(file.file_path.as_str())).then_some(id),
                // As in a write: the batch stops before touching the file,
                // and what was restored so far is reported.
                Err(err) => {
                    report
                        .refused
                        .push(refuse(&format!("database error: {err}")));
                    report.cancelled = true;
                    break;
                }
            },
            None => None,
        };
        pause_if_playing(&engine, track_id).await;
        let file_edit = to_edit(&restore);
        let outcome = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                patch_file(&path, &TagPatch::Fields(&file_edit)).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("tag write join: {e}"))
            .and_then(|r| r)
        };
        if let Err(err) = outcome {
            report
                .refused
                .push(refuse(&format!("tag write failed: {err}")));
            continue;
        }
        // The old values are back in the file: recorded at once, so a
        // failure of the database below cannot leave the batch claiming
        // the file still holds what it wrote.
        let file_id = file.id;
        let recorded = enrichment::write_now(&pool, move |conn| {
            Box::pin(async move { journal::mark_undone(conn, file_id).await })
        })
        .await;
        report.restored += 1;
        if let Err(err) = recorded {
            // Restored, but still recorded as written: a later undo refuses
            // it, its bytes having changed. Said, and the batch stops.
            report
                .refused
                .push(refuse(&format!("database error: {err}")));
            report.cancelled = true;
            break;
        }
        if let Some(track_id) = track_id {
            let db_edit = to_db_edit(&restore, &path);
            let synced = async {
                retry_busy(|| rehash_track_file(&pool, track_id, &path)).await?;
                retry_busy(|| sync_db(&pool, track_id, &db_edit)).await
            }
            .await;
            // The file is restored and recorded as such — its bytes no
            // longer hash to what the batch left, so it could not be
            // undone again anyway. Its row is caught up by the next scan
            // of its folder: neither this update nor any tag write touches
            // `file_modified` / `file_size`, so the scan's fast path finds
            // them changed and reads the file back. Said; the batch is
            // settled from its journal all the same, since nothing of it is
            // left to undo.
            if let Err(err) = synced {
                tracing::warn!(track_id, %err, "tag workshop database update failed");
                report.refused.push(refuse(DATABASE_STALE));
            }
            let _ = app.emit("track:updated", track_id);
        }
    }
    if let Some(task) = &task {
        task.progress(total, total);
    }
    let now = now_ms();
    // Under another profile the batch cannot be settled; the files undone
    // so far are recorded, and the next undo of it finishes the job.
    report.done = !profile_gone
        && db
            .write(move |conn| {
                Box::pin(async move { journal::settle_undo(conn, batch_id, now).await })
            })
            .await
            .unwrap_or_else(|err| {
                // The files are restored; the batch is marked undone by the
                // next undo of it, which finds nothing left to do.
                tracing::warn!(batch_id, %err, "tag workshop could not settle the undo");
                false
            });
    drop(task);
    let _ = app.emit(CHANGED_EVENT, ());
    let _ = app.emit("library:rescanned", ());
    let _ = app.emit("player:queue-changed", ());
    if report.restored > 0 {
        state.drain.notify();
    }
    Ok(report)
}

// ---------------------------------------------------------------------
// MusicBrainz
// ---------------------------------------------------------------------

/// One file as the matcher reads it: the workshop's current values, so a
/// title corrected in the table is the title matched.
#[derive(Debug, Clone, Deserialize)]
pub struct MatchTrack {
    pub track_id: i64,
    pub title: String,
    pub duration_ms: Option<i64>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
}

/// Propose tags for `tracks` from MusicBrainz: from `release_mbid` when
/// the user named one, else from the release the files' tags name
/// (`tagged_release`), else from the best release a search finds — the
/// enrichment engine's own resolution. `None` when nothing fits.
#[tauri::command]
pub async fn tag_workshop_match(
    album: String,
    artist: Option<String>,
    tracks: Vec<MatchTrack>,
    release_mbid: Option<String>,
    tagged_release: Option<String>,
) -> AppResult<Option<AlbumProposal>> {
    if crate::offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on — turn it off to match with MusicBrainz".into(),
        ));
    }
    if tracks.is_empty() {
        return Ok(None);
    }
    let local = LocalAlbum {
        album_id: 0,
        title: album.trim().to_owned(),
        artist: artist
            .map(|a| a.trim().to_owned())
            .filter(|a| !a.is_empty()),
        tracks: tracks
            .into_iter()
            .map(|t| LocalTrack {
                track_id: t.track_id,
                signals: TrackSignals {
                    title: t.title,
                    duration_ms: t.duration_ms.filter(|d| *d > 0),
                    track_number: t.track_number.filter(|n| *n > 0),
                    disc_number: t.disc_number.filter(|n| *n > 0),
                },
                file_path: String::new(),
            })
            .collect(),
    };
    let (mb, _) = enrichment::clients();
    let found = match release_mbid
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        Some(mbid) => resolve::resolve_against_release(&mb, &local, &mbid.to_ascii_lowercase())
            .await
            .map_err(|e| AppError::Other(format!("musicbrainz: {e}")))?,
        None => resolve::resolve_album_release(&mb, &local, tagged_release.as_deref())
            .await
            .map_err(|e| AppError::Other(format!("musicbrainz: {e}")))?,
    };
    Ok(
        found
            .map(|(resolution, release)| proposal::propose(&local, &release, resolution.from_tags)),
    )
}

#[cfg(test)]
mod tests;
