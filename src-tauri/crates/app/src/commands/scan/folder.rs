//! Scanning a library folder: the command, the pipeline every scan entry
//! point routes through, and the event announcing each inserted track.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use sqlx::SqlitePool;
use tauri::Emitter;

use waveflow_core::repository::{album::AlbumRepository, sqlite::SqliteAlbumRepository};
use waveflow_core::scanner::{
    link_va_artist_image, maybe_link_artist_images, merge_implicit_compilations, now_millis,
    realign_album_by_identity, reattach_orphaned_play_events, refresh_folder_covers,
    repoint_fallback_album_artist, split_artist_name, upsert_album_with_artist_mbid,
    upsert_artwork, ArtistImageScanCache, ExtractedFile, UpsertCache,
};

use crate::{
    commands::artist_split::is_split_of,
    error::{AppError, AppResult},
    state::AppState,
};

use super::moved::relink_moved_files;
use super::*;

/// Walk an existing `library_folder` on disk, extract tags from every audio
/// file, and upsert them into the active profile's database.
///
/// New files are inserted, existing rows are updated in place (keying on
/// `(library_id, file_path)`), and files that haven't changed since the last
/// scan — matched on `(file_modified, file_hash)` — are skipped to keep the
/// loop fast on re-scans. Pass `deep: true` to bypass that fast path and
/// force a full re-hash + re-read of every file in the folder (issue #366)
/// — the only way to notice a tag/embedded-picture edit made by an
/// external tool that preserved the file's mtime.
///
/// Failures on individual files are logged but never abort the scan: the
/// summary counter `errors` surfaces them to the UI so the user can tell how
/// many files were rejected.
#[tauri::command]
pub async fn scan_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    folder_id: i64,
    // Issue #366, symptom A remainder: an external tool can rewrite a
    // tag or an embedded picture while preserving the file's mtime
    // (e.g. Mp3tag's "preserve modification time" option), which the
    // normal fast-path scan below never notices. `deep` bypasses it so
    // every file gets a full re-hash + re-read, at the cost of scan
    // time — an explicit, occasional user action, not the default.
    deep: Option<bool>,
) -> AppResult<ScanSummary> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let summary = scan_folder_inner(
        &pool,
        &artwork_dir,
        folder_id,
        Some(&app),
        deep.unwrap_or(false),
    )
    .await?;
    // Phase 4.d.0.3: wake the sync drain so the freshly enqueued
    // track ops ship immediately instead of waiting on the
    // drain's idle poll. Matches the convention every other
    // CRUD command in this crate already follows
    // (`commands/library.rs`, `commands/playlist.rs`).
    state.drain.notify();
    // Fire the auto-analyzer in the background when the user has
    // opted in. Spawned so the IPC reply doesn't block on a
    // potentially long analysis pass.
    // Not after a scan the user stopped. Some tracks did land, so
    // `added` is positive -- but answering a stop by starting the
    // library-wide analysis sweep is the opposite of what was asked,
    // and that sweep is the longest job in the app (a full Symphonia
    // decode of every track; issue #286). Same rule as the import
    // path in `commands/library.rs`.
    if summary.added > 0 && !summary.cancelled {
        crate::commands::analysis::maybe_auto_analyze(&app);
    }
    // Any scan, not only one that added files: a rewritten file loses
    // the genre the enrichment engine filled, and its pass fills it back.
    if !summary.cancelled {
        crate::commands::enrichment::maybe_run(&app);
    }
    Ok(summary)
}

/// Inner scan implementation shared between the `scan_folder` command and
/// the `rescan_library` command, which walks every folder of a library.
///
/// Takes the resolved database pool + artwork directory directly so it can
/// run in contexts where a `tauri::State` isn't available (e.g. called in a
/// loop from another command).
///
/// `deep` disables the Phase 1 `(mtime, size)` fast path below so every
/// file is re-hashed and re-read regardless — every caller except
/// `scan_folder` (issue #366's deep-rescan action) passes `false` to keep
/// the normal fast re-scan behaviour.
pub(crate) async fn scan_folder_inner(
    pool: &SqlitePool,
    artwork_dir: &Path,
    folder_id: i64,
    app_handle: Option<&tauri::AppHandle>,
    deep: bool,
) -> AppResult<ScanSummary> {
    // Mark a scan in flight for the whole body so the background
    // analyzer parks itself instead of contending on CPU + the single
    // SQLite writer. Dropped on every exit path (RAII).
    let _scan_guard = ScanInFlightGuard::new();

    // Announce the scan so the status bar can show it and offer a stop
    // (#601). Unlike the five operations that already had a `cancel_*`
    // command, this one had no mechanism of its own, so the registry's
    // own flag *is* the mechanism here — hence the `None` callback and
    // the `is_cancelling` polling further down.
    //
    // The handle retires the row on every exit path, including `?` and
    // panic. A scan that leaves a ghost row behind is worse than one
    // that reports nothing: the user gets a spinner that never ends and
    // a stop button that does nothing.
    let task = app_handle.and_then(|app| {
        crate::tasks::start(
            app,
            crate::tasks::TaskKind::LibraryScan,
            0,
            crate::tasks::Cancellation::Flag,
        )
    });

    // Belt-and-braces: the directory is created at profile bootstrap, but a
    // user fiddling with the data folder could have deleted it.
    std::fs::create_dir_all(artwork_dir)?;

    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT library_id, path FROM library_folder WHERE id = ?")
            .bind(folder_id)
            .fetch_optional(pool)
            .await?;
    let Some((library_id, folder_path)) = row else {
        return Err(AppError::Other(format!("folder {folder_id} not found")));
    };
    // What the folder makes available before the scan, to tell afterwards
    // whether a track came back — a file that reappears unchanged is
    // counted as skipped, not added.
    let available_before =
        crate::commands::playlist_import::available_in_folder(pool, folder_id).await;

    // Phase timers (diagnostics — logged once at the end so a slow scan
    // on a big library tells us which phase to optimise).
    let t_scan = Instant::now();

    // Walk the directory off-thread — walkdir is blocking, and a deep tree
    // on a network share can take minutes to enumerate. The task row goes
    // with it and comes back: the walk reports through it (files found,
    // the directory it is in, no total yet) and watches its stop flag.
    let folder_path_owned = folder_path.clone();
    let (walked, task, network) = tokio::task::spawn_blocking(move || {
        // A share is listed in its own way, and what it answers replaces the
        // `fs::metadata` calls and file opens below. Its walk gives a whole
        // list or none (`netlib::scan`): the sweep at the end of this function
        // must never run on part of one.
        if waveflow_core::network_locator::is_locator(&folder_path_owned) {
            let walked = crate::netlib::scan::walk(
                &folder_path_owned,
                |found, dir| {
                    if let Some(task) = task.as_ref() {
                        task.progress(found as u64, 0);
                        task.detail(dir);
                    }
                },
                || task.as_ref().is_some_and(|t| t.is_cancelling()),
            );
            return match walked {
                Ok(Some(listing)) => (
                    Ok(Some(FolderWalk {
                        files: listing.files.clone(),
                        unreadable: Vec::new(),
                    })),
                    task,
                    Some(Arc::new(listing)),
                ),
                Ok(None) => (Ok(None), task, None),
                Err(err) => (Err(format!("network share: {err}")), task, None),
            };
        }
        let walked = collect_scannable_files_with(
            Path::new(&folder_path_owned),
            |found, dir| {
                if let Some(task) = task.as_ref() {
                    task.progress(found as u64, 0);
                    task.detail(dir.to_string_lossy());
                }
            },
            || task.as_ref().is_some_and(|t| t.is_cancelling()),
        );
        (walked, task, None)
    })
    .await
    .map_err(|e| AppError::Other(format!("walk task failed: {e}")))?;
    // Nothing was read and nothing is written: a folder that does not answer
    // is not an empty folder. A share's whole list or none; a local folder's
    // root, a drive that is not plugged in. A local error names the folder and
    // reaches the user from here, whichever way the scan started: a rescan of
    // the library and the one at launch only count it.
    let walked = match walked {
        Ok(walked) => walked,
        Err(reason) if waveflow_core::network_locator::is_locator(&folder_path) => {
            return Err(AppError::Other(reason));
        }
        Err(reason) => {
            return Err(folder_unreachable(
                app_handle,
                folder_id,
                &folder_path,
                reason,
            ));
        }
    };
    let is_network = network.is_some();
    // A share going away is a stop too: every file left would wait out its
    // timeout to fail, and like a stop it leaves the sweep out.
    let cancelled = || {
        task.as_ref().is_some_and(|t| t.is_cancelling())
            || network.as_ref().is_some_and(|n| n.source.is_down())
    };
    let walk_ms = t_scan.elapsed().as_millis();

    // Stopped while listing: nothing was read and nothing is written. In
    // particular not the disappearance sweep, which would take every
    // track the unfinished list had not reached yet for a deleted file.
    // Asked once more on the way out: the walk only looks every few
    // entries, and a stop that lands after its last look with nothing
    // found would meet no loop to notice it before the sweep.
    let Some(FolderWalk {
        files: audio_files,
        unreadable,
    }) = walked.filter(|_| !cancelled())
    else {
        tracing::info!(folder_id, walk_ms, "scan stopped while listing files");
        let summary = ScanSummary {
            folder_id,
            cancelled: true,
            ..Default::default()
        };
        if let Some(app) = app_handle {
            let _ = app.emit(
                "scan:progress",
                ScanProgress {
                    folder_id,
                    current: 0,
                    total: 0,
                    added: 0,
                    updated: 0,
                    skipped: 0,
                    errors: 0,
                    done: true,
                    cancelled: true,
                    current_dir: None,
                },
            );
        }
        return Ok(summary);
    };

    let mut summary = ScanSummary {
        folder_id,
        ..Default::default()
    };
    let now = now_millis();

    // Snapshot of (path → (file_modified_ms, file_size)) for every
    // currently-available track in this folder.
    //
    // Two purposes:
    //   1. Re-scan fast path — if the file on disk still has the same
    //      mtime + size as the row, we skip the expensive `extract_file`
    //      (hash + tag re-read) entirely. Re-scans of an 800-track
    //      library drop from ~30 s to <1 s.
    //   2. Disappearance sweep — paths still in the map at the end were
    //      on disk last time but aren't now, so we mark them unavailable
    //      below.
    //
    // Tracks already at `is_available = 0` are excluded — bringing them
    // back is handled by the upsert path which re-sets the flag to 1.
    let existing_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT file_path, file_modified, file_size,
                (rg_track_gain_db IS NULL AND rg_track_peak IS NULL
                 AND rg_album_gain_db IS NULL AND rg_album_peak IS NULL) AS rg_missing
           FROM track
          WHERE folder_id = ? AND is_available = 1",
    )
    .bind(folder_id)
    .fetch_all(pool)
    .await?;
    let mut existing_meta: HashMap<String, (i64, i64)> = HashMap::new();
    let mut rg_missing: HashSet<String> = HashSet::new();
    for (path, mtime, size, missing) in existing_rows {
        if missing != 0 {
            rg_missing.insert(path.clone());
        }
        existing_meta.insert(path, (mtime, size));
    }

    // ReplayGain tags became a thing the scanner reads *after* these
    // rows were written, and the fast path below never opens a file
    // whose mtime + size still match — so on an existing library the
    // feature would stay invisible until someone ran a deep rescan.
    // One pass per folder re-reads the tracks that have no ReplayGain
    // columns yet; the marker below keeps it from repeating, which
    // matters because a library with no ReplayGain tags at all would
    // otherwise re-read every file on every scan and lose the fast
    // path permanently.
    //
    // Not on a share, for this pass and the others below: they exist for rows
    // older than a feature, a share's rows are not in any release, and each
    // pass would read every file of the folder again. A deep rescan reaches a
    // share's rows if one ever needs it.
    let rg_backfill = ForcedPass::load(
        pool,
        "rg_backfill",
        folder_id,
        is_network || rg_missing.is_empty(),
    )
    .await;

    // Custom tags (#588) land in `track_tag` at scan time, and every
    // file of a library that predates the feature still matches on
    // mtime and size — so the fast path would skip all of them and the
    // columns would stay empty until the user happened to run a deep
    // rescan of every folder. One forced pass per folder, marked so it
    // never repeats, in the shape the ReplayGain backfill above already
    // established.
    //
    // Unlike that one this cannot narrow to the affected rows: a track
    // with no custom tags has no `track_tag` rows either, so "has none"
    // and "was never read" are the same shape. The marker is the only
    // thing that separates a first pass from a second.
    //
    // A file whose tags cannot be read is usually transient -- an antivirus
    // holding it, a share that dropped -- so the pass is worth repeating. But
    // some are not: a truncated container fails identically every time, and
    // holding the marker back for it would make every future scan of that
    // folder re-read every file in it, for the life of the install. So it
    // retries a few times, then accepts the folder as read.
    let tag_backfill = ForcedPass::load(pool, "tag_backfill", folder_id, is_network).await;

    // Several genres per track (#729) are read since a library already
    // holds one per track — the first value, or a "Rock; Jazz" kept whole
    // — and the fast path never reopens a file whose mtime and size still
    // match. One forced pass per folder lets the unchanged-file branch
    // relink them, marked like the custom-tag pass so it never repeats.
    // A file that fails extraction holds the marker back, since a lock
    // is usually transient and its genres would otherwise stay stale for
    // good; but only for a few scans, as the custom-tag pass does, so a
    // permanently unreadable file does not re-read the folder forever.
    let genre_backfill = ForcedPass::load(pool, "genre_backfill", folder_id, is_network).await;

    // The track and disc totals (album completeness) are read since a
    // library already holds rows without them, and the fast path never
    // reopens an unchanged file: one forced pass per folder fills them,
    // with the genre pass's marker and retry budget.
    let totals_backfill = ForcedPass::load(pool, "totals_backfill", folder_id, is_network).await;

    // Every artist of a file is read since a library already holds the first
    // one alone, and the fast path never reopens an unchanged file. One forced
    // pass per folder lets the unchanged-file branch relink them, with the
    // same marker and retry budget.
    let artist_backfill = ForcedPass::load(pool, "artist_backfill", folder_id, is_network).await;

    let meta_load_ms = t_scan.elapsed().as_millis();

    // A share whose folder lists empty while the library holds tracks from it
    // is far more likely a volume the server has not mounted than an emptied
    // collection, and sweeping would hide every one of them.
    if is_network && audio_files.is_empty() && !existing_meta.is_empty() {
        return Err(AppError::Other(
            "network share: the folder is empty, and the library has tracks from it".into(),
        ));
    }
    // The same on a local disk, as far as one can tell. A drive that is not
    // mounted leaves its mount point behind on Linux, an ordinary empty
    // directory: a folder that was a mount point when it held files and is not
    // one now. Anything else that lists empty is what it looks like: somebody
    // emptied it, and its tracks go below.
    let was_mount_key = format!("scan.root_was_mount.{folder_id}");
    if !is_network && audio_files.is_empty() && !existing_meta.is_empty() {
        let was_mount = read_counter(pool, &was_mount_key, folder_id).await == 1;
        if drive_is_away(Path::new(&folder_path), was_mount) {
            return Err(folder_unreachable(
                app_handle,
                folder_id,
                &folder_path,
                "it is empty, and its files were on a drive that is not mounted".to_string(),
            ));
        }
    }
    if !is_network && !audio_files.is_empty() {
        let is_mount = i64::from(is_mount_point(Path::new(&folder_path)));
        if read_counter(pool, &was_mount_key, folder_id).await != is_mount {
            if let Err(err) = write_counter(pool, &was_mount_key, is_mount).await {
                tracing::warn!(?err, folder_id, "could not record the folder's mount point");
            }
        }
    }

    let total_files = audio_files.len();
    if let Some(task) = task.as_ref() {
        task.progress(0, total_files as u64);
        task.detail(crate::netlib::display_path(Path::new(&folder_path)));
    }

    // Initial tick so the frontend's progress toast can size itself
    // even before the first file is processed (helps when the loop is
    // mostly skips and would otherwise emit nothing for several
    // hundred files).
    if let Some(app) = app_handle {
        let _ = app.emit(
            "scan:progress",
            ScanProgress {
                folder_id,
                current: 0,
                total: total_files,
                added: 0,
                updated: 0,
                skipped: 0,
                errors: 0,
                done: false,
                cancelled: false,
                current_dir: None,
            },
        );
    }

    // ─── Phase 1: Serial fast-path classification ─────────────────
    // Cheap fs::metadata (one syscall per file) lets us short-circuit
    // re-scans where nothing changed. Files that survive are pushed
    // to `to_extract` for the parallel extraction phase.
    //
    // `deep` skips the mtime/size comparison entirely (issue #366) —
    // every file goes to `to_extract` and gets a full re-hash + re-read,
    // the only way to catch a tag/embedded-picture edit that didn't move
    // the file's mtime.
    //
    // Every path the walk actually found is removed from `existing_meta`
    // right here, unconditionally — before the `deep` check and before
    // knowing whether extraction will succeed. The disappearance sweep at
    // the end of this function treats anything still left in
    // `existing_meta` as gone from disk; a file that's merely queued for
    // (re-)extraction is not gone, and if that extraction later fails
    // (a lock, a transient I/O error, a malformed rewrite mid-scan), it
    // must not be swept into `is_available = 0` just because the
    // consumer loop's own `existing_meta.remove` — reached only on a
    // successful extraction — never ran for it. Deep mode makes this
    // matter far more: every file in the folder is now on the failure-
    // exposed path, not just the ones that happened to change.
    let mut to_extract: Vec<PathBuf> = Vec::with_capacity(audio_files.len());
    for (idx, path) in audio_files.into_iter().enumerate() {
        // Between two files is a safe stopping point for this half: the
        // triage loop only decides what to extract and has written
        // nothing. What it has *not* done is the important part — see
        // the guard on the missing-file sweep at the end.
        if cancelled() {
            summary.cancelled = true;
            break;
        }
        summary.scanned += 1;
        let path_str = path.to_string_lossy().into_owned();
        let stored = existing_meta.remove(&path_str);
        if !deep {
            if let Some((stored_mtime, stored_size)) = stored {
                // A share's listing already said, with no request per file.
                let current = match network.as_ref() {
                    Some(listing) => listing
                        .meta(&path_str)
                        .map(|meta| (meta.size as i64, meta.modified_ms)),
                    None => std::fs::metadata(&path).ok().map(|metadata| {
                        let modified = metadata
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        (metadata.len() as i64, modified)
                    }),
                };
                if let Some((disk_size, disk_mtime_ms)) = current {
                    let needs_rg_backfill = rg_backfill.pending && rg_missing.contains(&path_str);
                    if disk_size == stored_size
                        && disk_mtime_ms == stored_mtime
                        && !needs_rg_backfill
                        && !tag_backfill.pending
                        && !genre_backfill.pending
                        && !totals_backfill.pending
                        && !artist_backfill.pending
                    {
                        summary.skipped += 1;
                        maybe_emit_progress(
                            app_handle,
                            folder_id,
                            idx + 1,
                            total_files,
                            &summary,
                            Some(&path),
                        );
                        if let Some(task) = task.as_ref() {
                            // The count of files *finished*, not of
                            // files looked at. `idx` climbs past every
                            // path the walk touches, including the ones
                            // being queued for extraction -- and the
                            // extraction loop below then resumes from
                            // `summary.skipped`, which is lower. On any
                            // library where an unchanged file follows a
                            // changed one, that makes the bar run
                            // forward and then jump back. The status
                            // bar is the whole of #601; a bar that goes
                            // backwards is worse than no bar.
                            task.progress(summary.skipped as u64, total_files as u64);
                        }
                        continue;
                    }
                }
            }
        }
        to_extract.push(path);
    }
    // A directory the walk could not read matters when it holds tracks of this
    // folder: what is left in the map by now, and the sweep leaves them alone.
    // Each is an error the user is told of. One that holds none is nothing to
    // report, and the root of a drive always has one (`lost+found`, `System
    // Volume Information`): counted, it would be an error at every scan.
    let unread_dirs = unreadable
        .iter()
        .filter(|dir| {
            existing_meta
                .keys()
                .any(|path| missed(std::slice::from_ref(dir), Path::new(path)))
        })
        .count();
    summary.errors += unread_dirs as u32;
    let stat_ms = t_scan.elapsed().as_millis();
    let to_extract_count = to_extract.len();

    // Created here (before the probe) so the probe SELECT below is
    // counted in `db_us` like every other DB access.
    let timings = Arc::new(ScanTimings::default());

    // Probe map for the insert-vs-update decision in the consumer loop,
    // pre-loaded in one pass instead of a `SELECT … WHERE library_id = ?
    // AND file_path = ?` per extracted track. Scoped to exactly the
    // `to_extract` paths via an `IN (…)` list — NOT a full library-wide
    // load: an fs-watcher rescan of a single changed file in a
    // 4000-track library must not drag in every row. SQLite caps bound
    // params at 999 and `library_id` takes one slot, so the path list is
    // chunked well under that. Scoped by `library_id` (not folder), so a
    // file already known under another folder of the same library still
    // resolves and the loop UPDATEs (reassigning `folder_id`) instead of
    // hitting the `UNIQUE(library_id, file_path)` constraint. No
    // `is_available` filter — matches the old probe, so a vanished-then-
    // restored track is found and flipped back to available. Snapshot-
    // before-loop is safe: the walk yields distinct paths, so no row
    // inserted earlier in THIS scan is ever probed later.
    const PROBE_CHUNK: usize = 900;
    type ExistingTrackProbe = (i64, i64, String, i64, Option<i64>);
    let mut existing_probe: HashMap<String, ExistingTrackProbe> = if to_extract.is_empty() {
        HashMap::new()
    } else {
        let t_db = Instant::now();
        let paths: Vec<String> = to_extract
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut probe = HashMap::with_capacity(paths.len());
        for chunk in paths.chunks(PROBE_CHUNK) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT file_path, id, file_modified, file_hash, added_at, album_id
                   FROM track
                  WHERE library_id = ? AND file_path IN ({placeholders})"
            );
            let mut q = sqlx::query_as::<_, (String, i64, i64, String, i64, Option<i64>)>(
                sqlx::AssertSqlSafe(sql),
            )
            .bind(library_id);
            for p in chunk {
                q = q.bind(p);
            }
            for (path, id, mtime, hash, added_at, album_id) in q.fetch_all(pool).await? {
                probe.insert(path, (id, mtime, hash, added_at, album_id));
            }
        }
        timings
            .db_us
            .fetch_add(t_db.elapsed().as_micros() as u64, Ordering::Relaxed);
        probe
    };

    // A file with no row may be one that moved. Settled before anything is
    // read, so it then goes down the path of a row that exists and keeps what
    // the user set. Not on a share, whose rows have nothing to gain from it
    // and whose files cannot be asked whether they are gone. Non-fatal: the
    // files are then new tracks, as they were before this existed.
    if !is_network && !summary.cancelled {
        let new_paths: Vec<PathBuf> = to_extract
            .iter()
            .filter(|path| !existing_probe.contains_key(path.to_string_lossy().as_ref()))
            .cloned()
            .collect();
        if !new_paths.is_empty() {
            match relink_moved_files(pool, library_id, folder_id, new_paths).await {
                Ok(moved) => {
                    for file in moved {
                        existing_meta.remove(&file.old_path);
                        existing_probe.insert(
                            file.new_path,
                            (
                                file.track_id,
                                file.file_modified,
                                file.file_hash,
                                file.added_at,
                                file.album_id,
                            ),
                        );
                        summary.moved += 1;
                    }
                }
                Err(err) => tracing::warn!(?err, folder_id, "move detection failed (non-fatal)"),
            }
        }
    }
    let mut previous_album_ids = HashSet::new();

    // ─── Phase 2: Parallel extract + transactional DB writes ──────
    //
    // `extract_file` is CPU-bound (BLAKE3 hash + lofty tag read) and
    // I/O-bound (file open + parent-dir walk for cover sidecar).
    // Spawn N extractions in flight via `buffered`, where N is the
    // host's parallelism count — saturates a multi-core CPU on the
    // hash without overwhelming the kernel I/O scheduler.
    //
    // DB writes stay serial (one writer per SQLite WAL is a hard
    // constraint anyway) but ride inside a transaction that commits
    // every TX_BATCH rows. With synchronous=NORMAL + WAL, this drops
    // a 1k-track first scan from ~30 s to a few seconds — fsync per
    // checkpoint instead of per row.
    let parallelism = if is_network {
        // The wait is the network's, not the CPU's.
        crate::netlib::scan::CONCURRENT_EXTRACTIONS
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get().max(2))
            .unwrap_or(4)
    };
    const TX_BATCH: usize = 200;

    let extraction_stream = futures::stream::iter(to_extract)
        .map(|path: PathBuf| {
            let artwork_dir = artwork_dir.to_path_buf();
            let timings = Arc::clone(&timings);
            let p = path.clone();
            let network = network.clone();
            async move {
                let res = tokio::task::spawn_blocking(move || match network.as_ref() {
                    Some(listing) => extract_network_file(&p, listing, &artwork_dir, &timings),
                    None => extract_file(&p, &artwork_dir, &timings),
                })
                .await;
                (path, res)
            }
        })
        .buffered(parallelism);
    futures::pin_mut!(extraction_stream);

    let mut tx = pool.begin().await?;
    let mut tx_count: usize = 0;
    let mut processed: usize = summary.skipped as usize;
    // Memo for artist / genre id lookups across the whole scan — see
    // `UpsertCache`. Lives for the loop's duration so every track on
    // the same album / by the same artist reuses the first resolution.
    let mut upsert_cache = UpsertCache::default();
    // Per-scan memo for the sidecar-artist-image walk (the single
    // biggest scan cost) — threaded into `maybe_link_artist_images`.
    // Skips repeat `(artist, folder)` pairs AND caches each directory's
    // `read_dir` so shared ancestor folders are read once.
    let mut artist_image_cache = ArtistImageScanCache::default();

    // Shared progress tick for both the error arms and the success path,
    // so a run of failed extractions still advances the toast instead of
    // freezing the counter until the next success (#430). Captures only
    // Copy handles, so it stays a plain `Fn` and never touches `summary`.
    let emit_tick = |current: usize, summary: &ScanSummary, path: &Path| {
        maybe_emit_progress(
            app_handle,
            folder_id,
            current,
            total_files,
            summary,
            Some(path),
        );
    };

    // A track the backfill pass was meant to re-read but couldn't:
    // the marker below must not be written, or that file would keep
    // its empty ReplayGain columns for good.
    //
    // A directory the walk could not read holds every pass back the same way,
    // spending a retry: its files were not read, and once it can be they
    // would take the fast path and keep what they have.
    let walk_incomplete = unread_dirs > 0;
    let mut rg_backfill_failed = walk_incomplete;

    // Same idea for the custom-tag pass: a file it was meant to read
    // but could not must stop the marker being written, or that track
    // keeps empty columns for the life of the install — the pass never
    // runs twice.
    let mut tag_backfill_failed = walk_incomplete;
    let mut genre_backfill_failed = walk_incomplete;
    let mut totals_backfill_failed = walk_incomplete;
    let mut artist_backfill_failed = walk_incomplete;

    while let Some((path, result)) = extraction_stream.next().await {
        if cancelled() {
            summary.cancelled = true;
            // Deliberately dropping the stream with extractions still
            // in flight, rather than awaiting them first.
            //
            // Awaiting would not stop them: `spawn_blocking` work is
            // uncancellable, so those files get hashed either way and
            // the only question is whether the user waits for them.
            // And "drain the stream" is not the same as "await what
            // already started" -- `buffered` pulls a new path from the
            // source every time a slot frees, so draining to
            // completion would extract the whole remaining folder.
            // On a large library that turns the stop button into the
            // longest operation of the scan.
            //
            // What gets abandoned is bounded and harmless: at most
            // `parallelism` extractions, none of which touch SQLite
            // (`extract_file` reads the file, hashes it, and writes
            // hash-addressed artwork). So releasing `SCANS_IN_FLIGHT`
            // before they finish cannot cost analysis rows to writer
            // contention, which is the reason that counter exists. The
            // worst case is a few cover files with no row pointing at
            // them, which the artwork sweep already collects.
            //
            // Every `?` in this loop drops the stream the same way; a
            // stop is not a new shape of exit.
            break;
        }
        processed += 1;
        if let Some(task) = task.as_ref() {
            task.progress(processed as u64, total_files as u64);
        }
        let extracted = match result {
            Ok(Ok(e)) => e,
            Ok(Err(err)) => {
                tracing::warn!(path = %path.display(), error = %err, "extraction failed");
                if rg_backfill.pending && rg_missing.contains(path.to_string_lossy().as_ref()) {
                    rg_backfill_failed = true;
                }
                // Deliberately *not* blocking the custom-tag marker. A
                // file that failed extraction got nothing at all from
                // this scan, not merely its tags, and it is already
                // counted in `summary.errors`. Holding the marker back
                // for it would re-run the forced full-folder tag pass
                // on every scan for the rest of the install, because a
                // file that is permanently unreadable fails every time
                // -- one corrupt file quietly costing a whole folder
                // its fast path. The case that does block it is below:
                // extraction succeeded and only the tag read failed,
                // which nothing else reports and nothing else retries.
                genre_backfill_failed |= genre_backfill.pending;
                totals_backfill_failed |= totals_backfill.pending;
                artist_backfill_failed |= artist_backfill.pending;
                summary.errors += 1;
                emit_tick(processed, &summary, &path);
                continue;
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "extraction panicked");
                if rg_backfill.pending && rg_missing.contains(path.to_string_lossy().as_ref()) {
                    rg_backfill_failed = true;
                }
                genre_backfill_failed |= genre_backfill.pending;
                totals_backfill_failed |= totals_backfill.pending;
                artist_backfill_failed |= artist_backfill.pending;
                summary.errors += 1;
                emit_tick(processed, &summary, &path);
                continue;
            }
        };

        // A file whose custom tags could not be read is not a file
        // with no custom tags: `read_extra_tags` answers `None` only
        // for "could not read", and every container it has no reader
        // for answers `Some(vec![])` instead. `write_extra_tags`
        // already leaves the stored rows alone in that case -- but the
        // one-off pass below would still be marked done, and it never
        // runs twice, so those tracks would keep empty columns for the
        // life of the install. The same rule the ReplayGain backfill
        // beside it follows.
        if tag_backfill.pending && extracted.extra_tags.is_none() {
            tag_backfill_failed = true;
        }

        // Redundant with Phase 1's unconditional removal above (every
        // walked path is already gone from `existing_meta` by this
        // point) — left as a harmless no-op safety net rather than
        // relied upon.
        existing_meta.remove(&extracted.abs_path);

        // Time the serial DB work for this track (probe + upserts +
        // INSERT/UPDATE + the periodic commit). `existing_meta.remove`
        // above is an in-memory HashMap op, negligible. No `continue`
        // sits between here and the accumulate below, so one delta
        // covers the whole iteration's DB cost.
        let t_db = Instant::now();

        // Insert-vs-update probe, served from the pre-loaded
        // `existing_probe` map instead of a per-track SELECT. Carries
        // `added_at` so we preserve the row's original "first import"
        // timestamp on the wire — re-emits must NOT bump it (a peer
        // device receiving a re-emit op for an existing track would
        // otherwise see the file as "just added" in its `Recently
        // added` view). Brand-new track path keeps using `now`.
        let existing = existing_probe.get(&extracted.abs_path).cloned();

        if let Some((existing_track_id, mtime, ref hash, existing_added_at, old_album_id)) =
            existing
        {
            if let Some(id) = old_album_id {
                previous_album_ids.insert(id);
            }
            if mtime == extracted.modified_ms && hash == &extracted.hash {
                // Track content hasn't changed — backfill paths only.
                // See the historical comments at the top of this branch
                // for context on why these queries still run on a
                // hash-match (cover backfill, codec/key backfill,
                // multi-artist normalisation).
                if let Some(cover) = &extracted.cover_art {
                    let row: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
                        "SELECT t.album_id, al.artwork_id
                           FROM track t
                           LEFT JOIN album al ON al.id = t.album_id
                          WHERE t.id = ?",
                    )
                    .bind(existing_track_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if let Some((Some(aid), None)) = row {
                        let artwork_id =
                            upsert_artwork(&mut tx, &cover.hash, &cover.format, cover.source)
                                .await?;
                        sqlx::query(
                            "UPDATE album SET artwork_id = ?, artwork_source = ? WHERE id = ?",
                        )
                        .bind(artwork_id)
                        .bind(cover.source.as_str())
                        .bind(aid)
                        .execute(&mut *tx)
                        .await?;
                    }
                }

                // `is_available = 1` resurfaces a track whose file had
                // vanished (marked `is_available = 0` by a prior scan)
                // and has now reappeared BYTE-IDENTICAL. This skip branch
                // is the only path that handles an unchanged re-add — the
                // full-rewrite `else` branch also sets it, but a same
                // mtime+hash file never reaches there — so without this
                // the row stays hidden forever (issue #366, symptom B).
                sqlx::query(
                    "UPDATE track
                        SET bit_depth           = COALESCE(bit_depth, ?),
                            codec               = COALESCE(codec, ?),
                            musical_key         = COALESCE(musical_key, ?),
                            rating              = COALESCE(?, rating),
                            title_from_filename = ?,
                            rg_track_gain_db    = ?,
                            rg_track_peak       = ?,
                            rg_album_gain_db    = ?,
                            rg_album_peak       = ?,
                            track_total         = ?,
                            disc_total          = ?,
                            is_available        = 1
                      WHERE id = ?",
                )
                .bind(extracted.bit_depth)
                .bind(extracted.codec.as_deref())
                .bind(extracted.musical_key.as_deref())
                // On a deep rescan only, which the user asks for. It is the
                // one way to bring back a rating an older build moved up a
                // step: a scan read the MP3 frame through lofty's whole-star
                // conversion and stored that, while the file kept the byte,
                // and with the bytes unchanged nothing else reads it again.
                // Not on the forced passes above, which reach this branch
                // without being asked and have no business changing stars.
                // The same rule as the rewriting branch: a file with no
                // rating leaves the stored one alone.
                .bind(extracted.rating.filter(|_| deep && !is_network).map(i64::from))
                // The one backfill site for the flag, and the only one
                // that can reach a library scanned before the column
                // existed. Those rows took the `NOT NULL DEFAULT 0` of
                // the migration while their titles were in fact file
                // stems; bytes that never change never reach the
                // rewrite branch, so without this line they would stay
                // at `0` for the life of the install and the inventory
                // category the column exists for would keep reporting
                // zero -- the very fault it was added to repair.
                //
                // Assigned, not COALESCEd: the column is NOT NULL, so
                // it is never `NULL` to fall through.
                .bind(extracted.title_from_filename)
                // Assigned rather than COALESCEd: this branch runs on a
                // file whose bytes are unchanged but whose row may be
                // stale, and a tagger that *removed* a gain has to be
                // able to clear it.
                .bind(extracted.replay_gain.track_gain_db)
                .bind(extracted.replay_gain.track_peak)
                .bind(extracted.replay_gain.album_gain_db)
                .bind(extracted.replay_gain.album_peak)
                // Assigned too, and for the same two reasons: this is the
                // only branch a library scanned before the columns existed
                // reaches (on a deep rescan), and a tagger may have removed
                // a total.
                .bind(extracted.track_total)
                .bind(extracted.disc_total)
                .bind(existing_track_id)
                .execute(&mut *tx)
                .await?;

                // Genres too can be stale on an unchanged file: a library
                // scanned before #729 holds one genre per track, the first
                // value or a "Rock; Jazz" kept whole. Relinked when the set
                // the file names differs from the stored one, so the next
                // scan repairs it without rewriting anything else.
                let mut wanted_genres = upsert_cache.genre_list(&mut tx, &extracted.genres).await?;
                let mut stored_genres: Vec<i64> =
                    sqlx::query_scalar("SELECT genre_id FROM track_genre WHERE track_id = ?")
                        .bind(existing_track_id)
                        .fetch_all(&mut *tx)
                        .await?;
                wanted_genres.sort_unstable();
                stored_genres.sort_unstable();
                if wanted_genres != stored_genres {
                    link_genres(&mut tx, existing_track_id, &wanted_genres).await?;
                }

                let mut multi_artist_renormalised = false;
                let mut credit_identity_moved = false;
                if let Some(raw) = &extracted.artist {
                    let splits = split_artist_name(raw);
                    let current_count: i64 =
                        sqlx::query_scalar("SELECT COUNT(*) FROM track_artist WHERE track_id = ?")
                            .bind(existing_track_id)
                            .fetch_one(&mut *tx)
                            .await?;
                    // A comma-joined tag (`split_artist_name` yields one
                    // name) whose track already credits several artists is
                    // a deliberate user split (issue #396) — don't collapse
                    // it back into the phantom on an unchanged-file rescan.
                    // A brand-new scan never lands here (it starts at one
                    // phantom row), so this only ever protects a split the
                    // user (or the split_artist command) already made. The
                    // same when one value among several was split: the count
                    // alone no longer tells, the names do.
                    let user_split = (splits.len() == 1 && current_count > 1)
                        || (current_count as usize > splits.len()
                            && is_split_of(
                                &stored_credits(&mut tx, existing_track_id)
                                    .await?
                                    .into_iter()
                                    .map(|(_, name)| name)
                                    .collect::<Vec<_>>(),
                                &splits,
                            ));
                    // On a deep rescan, a file that names its artists by
                    // MusicBrainz id is held to them (#1076): this is how a
                    // library scanned before the ids were read, whose "LiSA"
                    // and "LISA" were one artist, comes apart. It also gives
                    // a row that holds no id yet the one its file carries,
                    // whether or not the credit then moves.
                    let identity_moved = if deep && !extracted.artist_mbids.is_empty() {
                        let wanted = upsert_cache
                            .artist_credit_list(&mut tx, &extracted.artist, &extracted.artist_mbids)
                            .await?;
                        let stored: Vec<i64> = sqlx::query_scalar(
                            "SELECT artist_id FROM track_artist
                              WHERE track_id = ? ORDER BY position",
                        )
                        .bind(existing_track_id)
                        .fetch_all(&mut *tx)
                        .await?;
                        wanted != stored
                    } else {
                        false
                    };
                    if !user_split && (current_count as usize != splits.len() || identity_moved) {
                        multi_artist_renormalised = true;
                        let ids = upsert_cache
                            .artist_credit_list(&mut tx, &extracted.artist, &extracted.artist_mbids)
                            .await?;
                        sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
                            .bind(existing_track_id)
                            .execute(&mut *tx)
                            .await?;
                        for (position, aid) in ids.iter().enumerate() {
                            sqlx::query(
                                "INSERT INTO track_artist (track_id, artist_id, role, position)
                                 VALUES (?, ?, 'main', ?)",
                            )
                            .bind(existing_track_id)
                            .bind(aid)
                            .bind(position as i64)
                            .execute(&mut *tx)
                            .await?;
                        }
                        let previous_primary: Option<i64> =
                            sqlx::query_scalar("SELECT primary_artist FROM track WHERE id = ?")
                                .bind(existing_track_id)
                                .fetch_one(&mut *tx)
                                .await?;
                        sqlx::query("UPDATE track SET primary_artist = ? WHERE id = ?")
                            .bind(ids.first().copied())
                            .bind(existing_track_id)
                            .execute(&mut *tx)
                            .await?;
                        // A track that changed artist by identity is realigned
                        // below, with its Album Artist: its album may hold the
                        // other artist's tracks, and must not move whole.
                        if let Some(first_id) = ids.first().copied() {
                            if !identity_moved {
                                repoint_fallback_album_artist(
                                    &mut tx,
                                    existing_track_id,
                                    previous_primary,
                                    first_id,
                                )
                                .await?;
                            }
                        }
                    }
                    credit_identity_moved = identity_moved;
                    // Backfill local artist images AFTER the optional
                    // track_artist rebuild — otherwise newly created
                    // artist IDs (when current_count != splits.len())
                    // would be skipped on first encounter. Cheap because
                    // already-linked artists are filtered by the
                    // `IS NOT NULL` pre-check inside the helper.
                    let track_path = Path::new(&extracted.abs_path);
                    let current_ids: Vec<i64> = sqlx::query_scalar(
                        "SELECT artist_id FROM track_artist
                          WHERE track_id = ? ORDER BY position",
                    )
                    .bind(existing_track_id)
                    .fetch_all(&mut *tx)
                    .await?;
                    let t_link = Instant::now();
                    maybe_link_artist_images(
                        &mut tx,
                        Some(raw),
                        &current_ids,
                        track_path,
                        artwork_dir,
                        &mut artist_image_cache,
                    )
                    .await?;
                    timings
                        .link_us
                        .fetch_add(t_link.elapsed().as_micros() as u64, Ordering::Relaxed);
                }

                // The album a fresh scan would build, on a deep rescan of a
                // file that names its artists by id (#1076): an older scan
                // keyed one album on the merged artist, whether it came from
                // the track's lead or from an explicit Album Artist.
                if deep && (credit_identity_moved || extracted.album_artist_mbid.is_some()) {
                    // The track's own picture, for an album it lands in
                    // without one: the old album's may be the other
                    // artist's record.
                    let own_cover = match &extracted.cover_art {
                        Some(cover) => Some((
                            upsert_artwork(&mut tx, &cover.hash, &cover.format, cover.source)
                                .await?,
                            cover.source.as_str(),
                        )),
                        None => None,
                    };
                    realign_album_by_identity(
                        &mut tx,
                        existing_track_id,
                        extracted.album_artist.as_deref(),
                        extracted.album_artist_mbid.as_deref(),
                        own_cover,
                    )
                    .await?;
                }

                // Phase 4.d.0.3: emit a sync op ONLY when the skip
                // branch actually rewrote multi-artist rows
                // (`track_artist` re-link from comma-joined → ";"-
                // split). Without this, a peer device that pulls
                // down the original track via sync would never
                // observe the multi-artist normalisation. Cover
                // and codec backfills don't need to round-trip
                // because they're server-derived metadata; only
                // the multi-artist relink is wire-visible.
                if multi_artist_renormalised {
                    emit_track_insert_from_extracted(
                        &mut tx,
                        library_id,
                        existing_track_id,
                        &extracted,
                        existing_added_at,
                    )
                    .await?;
                }

                // Written even here, and this is the point: a library
                // that predates #588 has files whose mtime and hash have
                // not moved, so every one of them lands in this branch.
                // A deep rescan re-reads the file -- `extracted` is
                // fresh -- and without this the tags it just read would
                // be thrown away and the columns would stay empty
                // forever.
                write_extra_tags(&mut tx, existing_track_id, extracted.extra_tags.as_ref()).await?;

                summary.skipped += 1;
                tx_count += 1;
            } else {
                // Hash or mtime changed → full re-write of the track row
                // and its many-to-many links. Falls through to the
                // shared insert/update path below.
                //
                // A split the user made on what the file still says stays:
                // this branch is what any rating or tag written from the app
                // leads to, so a file "changing" says nothing about its
                // credits. The album stays where the split left it too, keyed
                // on the artist the split did not touch.
                let stored = stored_credits(&mut tx, existing_track_id).await?;
                let tagged = extracted
                    .artist
                    .as_deref()
                    .map(split_artist_name)
                    .unwrap_or_default();
                let names: Vec<String> = stored.iter().map(|(_, name)| name.clone()).collect();
                let user_split = is_split_of(&names, &tagged);
                let artist_ids = if user_split {
                    stored.iter().map(|(id, _)| *id).collect()
                } else {
                    upsert_cache
                        .artist_credit_list(&mut tx, &extracted.artist, &extracted.artist_mbids)
                        .await?
                };
                let artist_id = artist_ids.first().copied();
                let album_key_artist = if user_split {
                    let keyed: Option<i64> =
                        sqlx::query_scalar("SELECT artist_id FROM album WHERE id = ?")
                            .bind(old_album_id)
                            .fetch_optional(&mut *tx)
                            .await?
                            .flatten();
                    keyed.or(artist_id)
                } else {
                    artist_id
                };
                let album_id = match &extracted.album {
                    Some(a) => {
                        upsert_album_with_artist_mbid(
                            &mut tx,
                            a,
                            extracted.album_artist.as_deref(),
                            extracted.album_artist_mbid.as_deref(),
                            extracted.is_compilation,
                            album_key_artist,
                            extracted.year,
                            &extracted.abs_path,
                        )
                        .await?
                    }
                    None => None,
                };
                let genre_ids = upsert_cache.genre_list(&mut tx, &extracted.genres).await?;

                let t_link = Instant::now();
                maybe_link_artist_images(
                    &mut tx,
                    extracted.artist.as_deref(),
                    &artist_ids,
                    Path::new(&extracted.abs_path),
                    artwork_dir,
                    &mut artist_image_cache,
                )
                .await?;
                timings
                    .link_us
                    .fetch_add(t_link.elapsed().as_micros() as u64, Ordering::Relaxed);

                sqlx::query(
                    "UPDATE track SET
                        folder_id = ?,
                        file_hash = ?, file_size = ?, file_modified = ?,
                        title = ?, title_from_filename = ?, album_id = ?, primary_artist = ?,
                        track_number = ?, disc_number = ?, year = ?,
                        track_total = ?, disc_total = ?,
                        duration_ms = ?, bitrate = ?, sample_rate = ?, channels = ?,
                        bit_depth = ?, codec = ?,
                        musical_key = ?,
                        -- Unlike the tags around it, the file is not the
                        -- only source of this one. A star can live in the
                        -- database alone (a `.dff`, a container whose reader
                        -- does not lift it), so a file with no rating has
                        -- said nothing about it, and that goes for one
                        -- another tagger emptied too: the two cannot be told
                        -- apart. A file that does carry a rating wins.
                        -- A rating set in WaveFlow on a share's track lives
                        -- here alone, since the file cannot be written: the
                        -- file changing on the share must not take it away.
                        rating = CASE WHEN ? THEN COALESCE(rating, ?) ELSE COALESCE(?, rating) END,
                        rg_track_gain_db = ?, rg_track_peak = ?,
                        rg_album_gain_db = ?, rg_album_peak = ?,
                        -- With `title`, never without it (#579): this is the
                        -- branch a retagged file takes, and the backfill only
                        -- ever looks at NULLs -- so a blob left behind here
                        -- would stay wrong for the life of the library.
                        pinyin = ?,
                        is_available = 1
                     WHERE id = ?",
                )
                .bind(folder_id)
                .bind(&extracted.hash)
                .bind(extracted.size)
                .bind(extracted.modified_ms)
                .bind(&extracted.title)
                .bind(extracted.title_from_filename)
                .bind(album_id)
                .bind(artist_id)
                .bind(extracted.track_number)
                .bind(extracted.disc_number)
                .bind(extracted.year)
                .bind(extracted.track_total)
                .bind(extracted.disc_total)
                .bind(extracted.duration_ms)
                .bind(extracted.bitrate)
                .bind(extracted.sample_rate)
                .bind(extracted.channels)
                .bind(extracted.bit_depth)
                .bind(extracted.codec.as_deref())
                .bind(extracted.musical_key.as_deref())
                .bind(is_network)
                .bind(extracted.rating.map(|r| r as i64))
                .bind(extracted.rating.map(|r| r as i64))
                .bind(extracted.replay_gain.track_gain_db)
                .bind(extracted.replay_gain.track_peak)
                .bind(extracted.replay_gain.album_gain_db)
                .bind(extracted.replay_gain.album_peak)
                .bind(waveflow_core::scanner::pinyin_blob(&extracted.title).unwrap_or_default())
                .bind(existing_track_id)
                .execute(&mut *tx)
                .await?;

                // A year the file carries is the file's, even one equal to
                // what the enrichment engine had filled: its provenance goes,
                // as `link_genres` does for genres, so an undo of the album
                // cannot clear it. A file with none leaves it.
                if let Some(year) = extracted.year {
                    waveflow_core::enrichment::store::release_track_fields(
                        &mut tx,
                        existing_track_id,
                        false,
                        true,
                    )
                    .await?;
                    if let Some(aid) = album_id {
                        waveflow_core::enrichment::store::release_album_years(
                            &mut tx,
                            &[aid],
                            year,
                        )
                        .await?;
                    }
                }

                if let (Some(cover), Some(aid)) = (&extracted.cover_art, album_id) {
                    let artwork_id =
                        upsert_artwork(&mut tx, &cover.hash, &cover.format, cover.source).await?;
                    // A changed file may carry a new embedded picture, and
                    // `artwork_id IS NULL` alone meant it never reached the
                    // album: the new image landed in the artwork dir and the
                    // album kept pointing at the old one (#750).
                    //
                    // Lifting the guard outright is not safe either. The tag
                    // editor stores a whole-file hash where the scanner keeps
                    // a head-and-tail one, so every rating or tag WaveFlow
                    // writes sends that file down this branch on the next
                    // scan — and on an album whose tracks carry different
                    // pictures, rating track 7 would hand the album track 7's.
                    //
                    // So an embedded album cover follows one track only: the
                    // album's first (lowest disc, then track number). A
                    // picture changed across the whole album reaches it
                    // through that track; one changed on track 7 does not.
                    // A cover chosen by hand, fetched from Deezer or found in
                    // the folder is never replaced from here.
                    //
                    // After the track row, not before: the first-track choice
                    // reads this track's album, disc and track number and
                    // availability, and a retagged file may have just changed
                    // any of them.
                    sqlx::query(
                        "UPDATE album SET artwork_id = ?, artwork_source = ?
                          WHERE id = ?
                            AND (artwork_id IS NULL
                                 OR (artwork_source = 'embedded'
                                     AND ? = 'embedded'
                                     AND ? = (SELECT t.id FROM track t
                                               WHERE t.album_id = ? AND t.is_available = 1
                                               ORDER BY COALESCE(t.disc_number, 1),
                                                        COALESCE(t.track_number, 2147483647),
                                                        t.file_path
                                               LIMIT 1)))",
                    )
                    .bind(artwork_id)
                    .bind(cover.source.as_str())
                    .bind(aid)
                    .bind(cover.source.as_str())
                    .bind(existing_track_id)
                    .bind(aid)
                    .execute(&mut *tx)
                    .await?;
                }

                write_extra_tags(&mut tx, existing_track_id, extracted.extra_tags.as_ref()).await?;

                sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
                    .bind(existing_track_id)
                    .execute(&mut *tx)
                    .await?;
                for (position, aid) in artist_ids.iter().enumerate() {
                    sqlx::query(
                        "INSERT INTO track_artist (track_id, artist_id, role, position)
                         VALUES (?, ?, 'main', ?)",
                    )
                    .bind(existing_track_id)
                    .bind(aid)
                    .bind(position as i64)
                    .execute(&mut *tx)
                    .await?;
                }

                link_genres(&mut tx, existing_track_id, &genre_ids).await?;

                // Phase 4.d.0.3: emit the track insert op so the
                // server's apply pipeline mirrors the update. The
                // upsert merges every scalar — wire-shape-identical
                // to the brand-new track branch below. Skipped
                // gracefully when sync isn't configured (no JWT /
                // Local mode).
                //
                // `added_at` carries the row's ORIGINAL import
                // timestamp, not `now` — a re-emit must not bump
                // the peer's `Recently added` ordering.
                emit_track_insert_from_extracted(
                    &mut tx,
                    library_id,
                    existing_track_id,
                    &extracted,
                    existing_added_at,
                )
                .await?;

                summary.updated += 1;
                tx_count += 1;
            }
        } else {
            // Brand-new track — insert with all related upserts.
            let artist_ids = upsert_cache
                .artist_credit_list(&mut tx, &extracted.artist, &extracted.artist_mbids)
                .await?;
            let artist_id = artist_ids.first().copied();
            let album_id = match &extracted.album {
                Some(a) => {
                    upsert_album_with_artist_mbid(
                        &mut tx,
                        a,
                        extracted.album_artist.as_deref(),
                        extracted.album_artist_mbid.as_deref(),
                        extracted.is_compilation,
                        artist_id,
                        extracted.year,
                        &extracted.abs_path,
                    )
                    .await?
                }
                None => None,
            };
            let genre_ids = upsert_cache.genre_list(&mut tx, &extracted.genres).await?;
            if let (Some(cover), Some(aid)) = (&extracted.cover_art, album_id) {
                let artwork_id =
                    upsert_artwork(&mut tx, &cover.hash, &cover.format, cover.source).await?;
                sqlx::query(
                    "UPDATE album SET artwork_id = ?, artwork_source = ?
                      WHERE id = ? AND artwork_id IS NULL",
                )
                .bind(artwork_id)
                .bind(cover.source.as_str())
                .bind(aid)
                .execute(&mut *tx)
                .await?;
            }

            let t_link = Instant::now();
            maybe_link_artist_images(
                &mut tx,
                extracted.artist.as_deref(),
                &artist_ids,
                Path::new(&extracted.abs_path),
                artwork_dir,
                &mut artist_image_cache,
            )
            .await?;
            timings
                .link_us
                .fetch_add(t_link.elapsed().as_micros() as u64, Ordering::Relaxed);

            let insert = sqlx::query(
                "INSERT INTO track (
                    library_id, folder_id, file_path, file_hash, file_size, file_modified,
                    title, title_from_filename, album_id, primary_artist,
                    track_number, disc_number, year, track_total, disc_total,
                    duration_ms, bitrate, sample_rate, channels,
                    bit_depth, codec, musical_key,
                    rating,
                    rg_track_gain_db, rg_track_peak, rg_album_gain_db, rg_album_peak,
                    added_at, pinyin, is_available
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                           ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)",
            )
            .bind(library_id)
            .bind(folder_id)
            .bind(&extracted.abs_path)
            .bind(&extracted.hash)
            .bind(extracted.size)
            .bind(extracted.modified_ms)
            .bind(&extracted.title)
            .bind(extracted.title_from_filename)
            .bind(album_id)
            .bind(artist_id)
            .bind(extracted.track_number)
            .bind(extracted.disc_number)
            .bind(extracted.year)
            .bind(extracted.track_total)
            .bind(extracted.disc_total)
            .bind(extracted.duration_ms)
            .bind(extracted.bitrate)
            .bind(extracted.sample_rate)
            .bind(extracted.channels)
            .bind(extracted.bit_depth)
            .bind(extracted.codec.as_deref())
            .bind(extracted.musical_key.as_deref())
            .bind(extracted.rating.map(|r| r as i64))
            .bind(extracted.replay_gain.track_gain_db)
            .bind(extracted.replay_gain.track_peak)
            .bind(extracted.replay_gain.album_gain_db)
            .bind(extracted.replay_gain.album_peak)
            .bind(now)
            // Written with the row, so a freshly scanned Chinese title is
            // searchable by pinyin without waiting for a backfill (#579).
            .bind(
                waveflow_core::scanner::pinyin_blob(&extracted.title).unwrap_or_default(),
            )
            .execute(&mut *tx)
            .await?;
            let track_id = insert.last_insert_rowid();

            for (position, aid) in artist_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO track_artist (track_id, artist_id, role, position)
                     VALUES (?, ?, 'main', ?)",
                )
                .bind(track_id)
                .bind(aid)
                .bind(position as i64)
                .execute(&mut *tx)
                .await?;
            }
            link_genres(&mut tx, track_id, &genre_ids).await?;

            // Phase 4.d.0.3: emit the track insert op for the
            // sync server. Sits inside the same tx as the entity
            // write — outbox rolls back with the track row if the
            // commit fails. Skipped gracefully when sync isn't
            // configured.
            write_extra_tags(&mut tx, track_id, extracted.extra_tags.as_ref()).await?;

            emit_track_insert_from_extracted(&mut tx, library_id, track_id, &extracted, now)
                .await?;

            summary.added += 1;
            tx_count += 1;
        }

        // Periodic commit so the WAL doesn't grow unbounded on big
        // first scans, AND so a failure mid-scan loses at most
        // TX_BATCH rows of work instead of the whole import.
        if tx_count >= TX_BATCH {
            tx.commit().await?;
            tx = pool.begin().await?;
            tx_count = 0;
        }

        timings
            .db_us
            .fetch_add(t_db.elapsed().as_micros() as u64, Ordering::Relaxed);

        emit_tick(processed, &summary, &path);
    }

    // Time the final commit into `db_us` too — the periodic TX_BATCH
    // commits are already counted inside the loop, so without this the
    // last batch's commit (which can carry up to TX_BATCH rows of fsync)
    // would be the one slice of DB cost `db_ms_total` silently omits.
    let t_final_commit = Instant::now();
    tx.commit().await?;
    timings.db_us.fetch_add(
        t_final_commit.elapsed().as_micros() as u64,
        Ordering::Relaxed,
    );
    let extract_db_ms = t_scan.elapsed().as_millis();

    // Anything still in the map was on disk last time but isn't now.
    // Mark it unavailable rather than deleting — preserves play_event
    // history and lets the user "undelete" by restoring the file.
    //
    // **Skipped entirely on a cancelled scan** (#601). The map is
    // "everything the walk has not reached", and after a stop that is
    // most of the folder — running the sweep would mark a working
    // library unavailable because the user pressed a stop button. This
    // is the concrete shape of "cancelling must leave consistent
    // state": the safe stopping point for a scan is not merely between
    // two files, it is *before this pass*.
    // SQLite caps bound parameters at ~999, so we update one row at a
    // time. Removed counts are normally tiny (a handful per scan); for
    // bulk wipes the loop is still acceptable since we're already
    // off the audio thread.
    let sweep: Vec<&String> = if summary.cancelled {
        Vec::new()
    } else {
        // A directory the walk could not read says nothing about its files.
        existing_meta
            .keys()
            .filter(|path| !network.as_ref().is_some_and(|n| n.is_under_refused(path)))
            .filter(|path| !missed(&unreadable, Path::new(path.as_str())))
            .collect()
    };
    for missing_path in sweep {
        let res = sqlx::query(
            "UPDATE track SET is_available = 0
              WHERE folder_id = ? AND file_path = ? AND is_available = 1",
        )
        .bind(folder_id)
        .bind(missing_path)
        .execute(pool)
        .await?;
        if res.rows_affected() > 0 {
            summary.removed += 1;
        }
    }

    sqlx::query("UPDATE library_folder SET last_scanned_at = ? WHERE id = ?")
        .bind(now)
        .bind(folder_id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE library SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(library_id)
        .execute(pool)
        .await?;

    // Auto-detect compilations among the tagless rows. Catches the case
    // where a Various-Artists record has neither aART nor TCMP set on
    // its source files (Soothing-Breeze-style lofi compilations,
    // SoundCloud DL packs, …) — the scanner would otherwise leave 21
    // single-track album rows fragmented by primary_artist.
    if let Err(err) = merge_implicit_compilations(pool).await {
        tracing::warn!(?err, "merge_implicit_compilations failed (non-fatal)");
    }

    // Album years are derived from the final set of available tracks. A
    // previously missing or corrected year must survive a rescan, while
    // conflicting track years must not depend on scan order (#816). Keep
    // this on cancellation too: processed tracks are already committed.
    let previous_album_ids: Vec<i64> = previous_album_ids.into_iter().collect();
    let album_repo = SqliteAlbumRepository::new(pool.clone());
    if let Err(err) = album_repo
        .refresh_years_after_folder_scan(folder_id, &previous_album_ids)
        .await
    {
        tracing::warn!(?err, "refresh_album_years failed (non-fatal)");
    }

    // VA is an album artist (never in `track_artist`), so the per-track
    // `maybe_link_artist_images` pass above can't reach it — resolve a
    // curated `Various Artists/artist.jpg` via the album relationship here
    // (issue #292). Runs after the merge so a freshly promoted VA row is
    // covered. Non-fatal: a missing sidecar is the common case.
    match pool.acquire().await {
        Ok(mut conn) => {
            if let Err(err) = link_va_artist_image(&mut conn, artwork_dir).await {
                tracing::warn!(?err, "link_va_artist_image failed (non-fatal)");
            }
        }
        Err(err) => tracing::warn!(?err, "link_va_artist_image: acquire failed (non-fatal)"),
    }

    // Pick up sidecar cover art that changed since the last scan. The
    // per-file fast path can't see this on its own (issue #366) — see
    // `refresh_folder_covers`. Non-fatal: a scan that indexed every
    // track correctly must not fail because one cover couldn't be read.
    let mut refreshed_covers = match refresh_folder_covers(pool, artwork_dir, folder_id).await {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(?err, "refresh_folder_covers failed (non-fatal)");
            0
        }
    };
    // A share's albums are the listing's to answer for. Not after a stop: what
    // is left could be reading from a share that went away.
    if let Some(listing) = network.as_ref().filter(|_| !summary.cancelled) {
        match crate::netlib::scan::refresh_covers(
            pool,
            listing,
            artwork_dir,
            folder_id,
            deep,
            &cancelled,
        )
        .await
        {
            Ok(n) => refreshed_covers += n,
            Err(err) => tracing::warn!(?err, "network refresh_covers failed (non-fatal)"),
        }
    }

    // Give listening history back to tracks that were deleted and have
    // now been re-scanned (issue #367). Runs library-wide rather than
    // per-folder: an orphaned event has no folder any more, that's what
    // being orphaned means. Non-fatal for the same reason as above.
    let reattached_plays = match reattach_orphaned_play_events(pool).await {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(?err, "reattach_orphaned_play_events failed (non-fatal)");
            0
        }
    };

    // The same, for imported playlists: a track the scan brought in takes
    // the place of every placeholder waiting for it. Library-wide too — a
    // playlist belongs to no folder. Non-fatal for the same reason. Only
    // when the scan changed what the library holds: matching reads the
    // whole library, and a watcher rescan that found nothing new would
    // pay that for nothing.
    let brought_tracks = summary.added + summary.updated + summary.moved > 0
        || crate::commands::playlist_import::available_in_folder(pool, folder_id).await
            != available_before;
    if brought_tracks {
        crate::commands::playlist_import::fill_placeholders_after_scan(pool, app_handle).await;
    }

    // Marked whatever the individual files turned out to hold: the
    // point of the pass is that every file was *read once*, and a
    // folder of tracks that carry no custom tags must not be re-read on
    // every future scan for the rest of the install's life.
    //
    // Not marked when the scan was stopped, though — a cancelled pass
    // did not reach most of the folder, and recording it as done would
    // leave those files without their tags permanently. A stop does not
    // spend one of the folder's retries either.
    tag_backfill
        .settle(pool, folder_id, summary.cancelled, tag_backfill_failed)
        .await;

    genre_backfill
        .settle(pool, folder_id, summary.cancelled, genre_backfill_failed)
        .await;
    totals_backfill
        .settle(pool, folder_id, summary.cancelled, totals_backfill_failed)
        .await;
    artist_backfill
        .settle(pool, folder_id, summary.cancelled, artist_backfill_failed)
        .await;

    // The ReplayGain pass the same way. A file that failed extraction this
    // time (locked, unreadable, mid-write) is picked up by the next scan, a few
    // times, rather than written off; the marker is still set when a file
    // turned out to carry no ReplayGain tags at all, since re-reading those on
    // every future scan is exactly what it exists to prevent.
    rg_backfill
        .settle(pool, folder_id, summary.cancelled, rg_backfill_failed)
        .await;

    // Per-phase breakdown so a slow scan on a big library is diagnosable
    // from the log alone. `*_ms` are wall-clock deltas between phases;
    // `hash_cpu_ms_total` / `tag_cpu_ms_total` are summed across the
    // `parallelism` extraction threads, so compare them to
    // `extract_db_ms * parallelism` to tell hash-bound from tag-bound.
    tracing::info!(
        folder_id,
        library_id,
        scanned = summary.scanned,
        added = summary.added,
        updated = summary.updated,
        skipped = summary.skipped,
        removed = summary.removed,
        moved = summary.moved,
        errors = summary.errors,
        refreshed_covers,
        reattached_plays,
        extracted = to_extract_count,
        parallelism,
        walk_ms,
        meta_load_ms = meta_load_ms.saturating_sub(walk_ms),
        stat_ms = stat_ms.saturating_sub(meta_load_ms),
        extract_db_ms = extract_db_ms.saturating_sub(stat_ms),
        post_ms = t_scan.elapsed().as_millis().saturating_sub(extract_db_ms),
        total_ms = t_scan.elapsed().as_millis(),
        hash_cpu_ms_total = timings.hash_us.load(Ordering::Relaxed) / 1000,
        tag_cpu_ms_total = timings.tag_us.load(Ordering::Relaxed) / 1000,
        extra_tag_cpu_ms_total = timings.extra_tag_us.load(Ordering::Relaxed) / 1000,
        // Serial single-writer DB time — already wall-clock (one
        // consumer task). The slice of `extract_db_ms` NOT covered by
        // this is the parallel extraction (hash + cover) the consumer
        // waited on.
        db_ms_total = timings.db_us.load(Ordering::Relaxed) / 1000,
        // Subset of db_ms_total spent in the sidecar-artist-image walk.
        link_ms_total = timings.link_us.load(Ordering::Relaxed) / 1000,
        "scan complete"
    );

    if let Some(app) = app_handle {
        let _ = app.emit(
            "scan:progress",
            ScanProgress {
                folder_id,
                // The files actually dealt with, not the whole folder:
                // a stopped scan sitting at 100% is the same lie as the
                // title one line down.
                //
                // `processed` and not `summary.scanned`, which counts
                // the triage loop -- and that loop runs to completion
                // before extraction starts, so for any stop taken
                // during extraction, where the time actually goes, it
                // already equals `total_files`.
                current: if summary.cancelled {
                    processed
                } else {
                    total_files
                },
                total: total_files,
                added: summary.added,
                updated: summary.updated,
                skipped: summary.skipped,
                errors: summary.errors,
                done: true,
                cancelled: summary.cancelled,
                current_dir: None,
            },
        );
    }

    Ok(summary)
}

/// Folders refused before anything could hear of it, `None` once something
/// does: the scan at launch can fail in milliseconds, ahead of the window.
static UNREACHABLE_UNHEARD: std::sync::Mutex<Option<Vec<String>>> =
    std::sync::Mutex::new(Some(Vec::new()));

/// What the notice toast asks for when it starts listening, and from then on
/// it hears the event.
#[tauri::command]
pub fn take_unreachable_folders() -> Vec<String> {
    UNREACHABLE_UNHEARD
        .lock()
        .ok()
        .and_then(|mut unheard| unheard.take())
        .unwrap_or_default()
}

/// The refusal of a folder that does not answer. Announced as well as
/// returned: a rescan of the library, the scan at launch and the watcher only
/// count or log what they get back, so the toast is what tells the user.
fn folder_unreachable(
    app_handle: Option<&tauri::AppHandle>,
    folder_id: i64,
    path: &str,
    reason: String,
) -> AppError {
    tracing::warn!(folder_id, path, %reason, "library folder cannot be read; nothing changed");
    if let Ok(mut unheard) = UNREACHABLE_UNHEARD.lock() {
        if let Some(unheard) = unheard.as_mut().filter(|u| !u.iter().any(|p| p == path)) {
            unheard.push(path.to_string());
        }
    }
    if let Some(app) = app_handle {
        let _ = app.emit(
            "library:folder-unreachable",
            serde_json::json!({ "folder_id": folder_id, "path": path }),
        );
    }
    AppError::FolderUnreachable {
        path: path.to_string(),
        reason,
    }
}

/// A track's credits as they stand, in order.
async fn stored_credits(
    tx: &mut sqlx::SqliteConnection,
    track_id: i64,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT ta.artist_id, a.name
           FROM track_artist ta
           JOIN artist a ON a.id = ta.artist_id
          WHERE ta.track_id = ?
          ORDER BY ta.position",
    )
    .bind(track_id)
    .fetch_all(&mut *tx)
    .await
}

/// Sync-emit shim — converts an `ExtractedFile` into the wire shape
/// the server's `apply::track::insert` handler expects (phase
/// 4.d.0.2) and enqueues the op via the standard outbox path.
///
/// Inlined here rather than in `sync::track_emit` because the
/// scanner is the only call site that operates on
/// `ExtractedFile`; the lower-level helper takes a borrowed
/// `TrackInsertWire` so other call sites (duplicates UI, future
/// importers) build it from their own context.
async fn emit_track_insert_from_extracted(
    tx: &mut sqlx::SqliteConnection,
    library_id: i64,
    track_id: i64,
    extracted: &ExtractedFile,
    added_at: i64,
) -> AppResult<()> {
    // The scanner's `; `-split convention drives the wire-shape
    // `artists` array. Position derives from the index after the
    // split, matching the server's `track_artist.position`
    // semantic.
    let artists: Vec<String> = match &extracted.artist {
        Some(s) => split_artist_name(s),
        None => Vec::new(),
    };
    let wire = crate::sync::track_emit::TrackInsertWire {
        file_hash: &extracted.hash,
        title: &extracted.title,
        file_size: extracted.size,
        file_modified: extracted.modified_ms,
        duration_ms: extracted.duration_ms,
        track_number: extracted.track_number,
        disc_number: extracted.disc_number,
        year: extracted.year,
        bitrate: extracted.bitrate,
        sample_rate: extracted.sample_rate,
        channels: extracted.channels,
        bit_depth: extracted.bit_depth,
        codec: extracted.codec.as_deref(),
        musical_key: extracted.musical_key.as_deref(),
        added_at,
        album_title: extracted.album.as_deref(),
        album_artist_name: extracted.album_artist.as_deref(),
        is_compilation: extracted.is_compilation,
        artists: &artists,
    };
    crate::sync::track_emit::emit_track_insert_in_tx(
        tx,
        library_id,
        track_id,
        &extracted.abs_path,
        &wire,
    )
    .await?;
    Ok(())
}
