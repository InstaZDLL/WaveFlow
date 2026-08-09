//! Maintenance commands. Bulk operations a user can trigger from the
//! Settings screen (regenerate thumbnails, prune orphan covers, …).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tauri::AppHandle;

use crate::{
    audio::AudioEngine,
    commands::profile_io::{
        read_include_metadata_artwork, write_archive, ArchiveManifest, ARCHIVE_VERSION,
    },
    error::{AppError, AppResult},
    state::AppState,
};

/// Walk every artwork directory the app owns (the shared metadata cache
/// + every per-profile cache) and (re)build the `_1x.jpg` / `_2x.jpg`
/// thumbnails for any full-size cover that doesn't have them yet.
///
/// Returns the number of source images successfully (re)processed.
#[tauri::command]
pub async fn regenerate_thumbnails(state: tauri::State<'_, AppState>) -> AppResult<u32> {
    let mut total: u32 = 0;

    // `regen_in_dir` is intentionally synchronous (walks the directory
    // with `std::fs`, decodes JPEGs/PNGs via the `image` crate, calls into
    // `fast_image_resize` and writes results). Calling it directly from
    // this async command would block the tokio runtime for as long as the
    // pass takes — easily several seconds on a populated library — and
    // stall every other command queued behind it. Run each batch through
    // `spawn_blocking` so the runtime stays responsive.
    let metadata_dir = state.paths.metadata_artwork_dir.clone();
    let metadata_total = tokio::task::spawn_blocking(move || regen_in_dir(&metadata_dir))
        .await
        .map_err(|e| AppError::Other(format!("regen_thumbnails join: {e}")))??;
    total = total.saturating_add(metadata_total);

    let profile_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM profile")
        .fetch_all(&state.app_db)
        .await
        .unwrap_or_default();
    for pid in profile_ids {
        let dir = state.paths.profile_artwork_dir(pid);
        let profile_total = tokio::task::spawn_blocking(move || regen_in_dir(&dir))
            .await
            .map_err(|e| AppError::Other(format!("regen_thumbnails join: {e}")))??;
        total = total.saturating_add(profile_total);
    }

    Ok(total)
}

/// Result of [`prune_cached_album_covers`] — surfaced to the Settings UI.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumCoverCleanup {
    /// `metadata_album` rows whose `cover_hash` was cleared.
    pub rows_cleared: u64,
    /// Cache files (`.jpg` + `_1x`/`_2x` thumbnails) deleted.
    pub files_deleted: u64,
    /// Bytes reclaimed on disk.
    pub bytes_freed: u64,
}

/// One-time cleanup companion to issue #493: drop the Deezer album covers the
/// shared cache accumulated for albums that already have their own local
/// artwork. Those files were downloaded as a side effect of opening an album
/// page (which only reads `label`/`release_date`) and were never displayed.
///
/// **Cross-profile safe.** The metadata cache is app-wide (one row per
/// `deezer_id`, shared across every profile), so a cover may be pruned only
/// when its album has local art in EVERY profile that carries it — an album
/// still art-less in *some* profile keeps its cover so that profile isn't left
/// blank (offline especially). And the cache is content-addressed + shared with
/// artist pictures, so a hash is unlinked from disk only once it is referenced
/// by NO remaining `metadata_album.cover_hash`, `metadata_artist.picture_hash`
/// or `background_hash`. The `UPDATE` + reference re-check run in a single
/// transaction for a consistent snapshot.
#[tauri::command]
pub async fn prune_cached_album_covers(
    state: tauri::State<'_, AppState>,
) -> AppResult<AlbumCoverCleanup> {
    let artwork_dir = state.paths.metadata_artwork_dir.clone();

    // 1. `deezer_id`s that must KEEP their cover: any album still WITHOUT local
    //    art in ANY profile. The cache is app-wide, so a single-profile view
    //    isn't enough — read every profile's `album` table. Open read-write but
    //    WITHOUT running migrations (no migrator call); a read-only open of a
    //    WAL db with no live writer can fail to create its `-shm`.
    // Propagate a read failure — an empty list here would leave `needed` empty
    // and mark EVERY cached cover prunable, mass-deleting covers other profiles
    // still need. Bail instead.
    let profile_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM profile")
        .fetch_all(&state.app_db)
        .await?;
    let mut needed: HashSet<i64> = HashSet::new();
    for pid in &profile_ids {
        let path = state.paths.profile_db(*pid);
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false);
        let ppool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| AppError::Other(format!("open profile {pid} db: {e}")))?;
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT deezer_id FROM album WHERE artwork_id IS NULL AND deezer_id IS NOT NULL",
        )
        .fetch_all(&ppool)
        .await
        .map_err(|e| AppError::Other(format!("read profile {pid} albums: {e}")))?;
        needed.extend(ids);
        ppool.close().await;
    }

    // 2. Candidate covers: every cached album cover whose album is art-having in
    //    every profile that has it (its `deezer_id` is not in `needed`). Queried
    //    straight off `app.db`.
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT deezer_id, cover_hash FROM metadata_album WHERE cover_hash IS NOT NULL",
    )
    .fetch_all(&state.app_db)
    .await?;
    let prunable: Vec<(i64, String)> = rows
        .into_iter()
        .filter(|(did, _)| !needed.contains(did))
        .collect();
    if prunable.is_empty() {
        return Ok(AlbumCoverCleanup {
            rows_cleared: 0,
            files_deleted: 0,
            bytes_freed: 0,
        });
    }
    let prune_ids: Vec<i64> = prunable.iter().map(|(d, _)| *d).collect();
    let candidate_hashes: HashSet<String> = prunable.into_iter().map(|(_, h)| h).collect();

    // 3. Clear those covers and re-check remaining references in ONE
    //    transaction, so the reference set is consistent with the clear.
    let mut tx = state.app_db.begin().await?;
    let mut rows_cleared = 0u64;
    for chunk in prune_ids.chunks(400) {
        let placeholders = std::iter::repeat("?")
            .take(chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        // SQL-safe: only bind placeholders are interpolated (never user data),
        // mirroring the ATTACH statement in `db::profile_db`.
        let sql = format!(
            "UPDATE metadata_album SET cover_hash = NULL WHERE deezer_id IN ({placeholders})"
        );
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for id in chunk {
            q = q.bind(*id);
        }
        rows_cleared += q.execute(&mut *tx).await?.rows_affected();
    }
    let referenced: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT cover_hash FROM metadata_album WHERE cover_hash IS NOT NULL
         UNION SELECT picture_hash FROM metadata_artist WHERE picture_hash IS NOT NULL
         UNION SELECT background_hash FROM metadata_artist WHERE background_hash IS NOT NULL",
    )
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    tx.commit().await?;

    let deletable: Vec<String> = candidate_hashes
        .into_iter()
        .filter(|h| !referenced.contains(h))
        .collect();

    // Blocking fs work off the runtime.
    let (files_deleted, bytes_freed) =
        tokio::task::spawn_blocking(move || delete_cover_files(&artwork_dir, &deletable))
            .await
            .map_err(|e| AppError::Other(format!("prune_cached_album_covers join: {e}")))?;

    Ok(AlbumCoverCleanup {
        rows_cleared,
        files_deleted,
        bytes_freed,
    })
}

/// Delete `<hash>.jpg` + `<hash>_1x.jpg` + `<hash>_2x.jpg` for each hash,
/// summing the bytes actually reclaimed. Best-effort per file (a locked /
/// missing file is skipped, not fatal).
fn delete_cover_files(dir: &Path, hashes: &[String]) -> (u64, u64) {
    let mut files_deleted = 0u64;
    let mut bytes_freed = 0u64;
    for hash in hashes {
        for name in [
            format!("{hash}.jpg"),
            format!("{hash}_1x.jpg"),
            format!("{hash}_2x.jpg"),
        ] {
            let path = dir.join(name);
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(&path).is_ok() {
                files_deleted += 1;
                bytes_freed += len;
            }
        }
    }
    (files_deleted, bytes_freed)
}

/// Factory reset. Wipes every profile, library, playlist, cache and
/// app-wide setting, then restarts the binary into a fresh
/// onboarding flow.
///
/// The frontend gates this behind a "type RESET to confirm" modal
/// (see [`ResetAppModal`](../../../../../src/components/common/ResetAppModal.tsx)),
/// so the command itself trusts that the user already confirmed and
/// proceeds without a second prompt.
///
/// Order matters here:
///
/// 1. Silence the cpal output immediately by flipping
///    `paused_output`. The rtrb ring still holds a few hundred ms
///    of decoded samples from before the reset; without this the
///    callback flushes them to the device during step 2's wait,
///    producing a jarring tail at the worst possible moment. Same
///    mechanism the window-close handler uses in `lib.rs`. No
///    paired restore is needed because step 5 below replaces the
///    process unconditionally — the in-memory engine state goes
///    with it.
/// 2. `engine.stop_and_wait` — fire `AudioCmd::Stop` AND await the
///    decoder thread's transition back to `Idle`. The decoder
///    publishes the `Idle` state only after it drops the active
///    stream (closing the `File` / `HttpMediaSource` handle), so
///    once this returns we know nothing audio-side is holding a
///    file open under the data dir. Without this wait the
///    `remove_dir_all` below races the decoder on Windows and the
///    currently-playing track's file refuses to delete. A 2 s
///    timeout is a generous upper bound for the cmd_rx → drop
///    cycle; if the decoder is stuck we log and proceed anyway,
///    because waiting forever serves no one — and step 1 already
///    muted the device so any straggling samples stay inaudible.
/// 3. Close the active profile pool, then `app.db`. On Windows the
///    SQLite WAL keeps the database file locked while a pool is
///    open; we MUST drain the pools before deleting the data dir.
/// 4. `remove_dir_all` the entire `AppPaths::root`. Run it on the
///    blocking pool — recursive directory deletion across a
///    populated install (thousands of artwork files + WAL files)
///    can take a noticeable fraction of a second and would stall
///    the runtime if done in-place. Treat `NotFound` as a no-op
///    (already-reset / install half-broken).
/// 5. `app.restart()` swaps the process unconditionally — including
///    on a partial-wipe failure or a spawn_blocking join error.
///    Returning `Err` here would leave the app in a zombie state:
///    `state.profile` is `None`, `state.app_db` is closed, and
///    `AppState::app_db` is a plain `SqlitePool` (no `RwLock`
///    wrapper) so it can't be reopened from a command. Restarting
///    is the only deterministic recovery — the bootstrap pass
///    re-creates a fresh "Default" profile on whatever survived
///    on disk and the user lands back in onboarding. Errors are
///    logged before the restart so the user-facing report still
///    captures what went wrong.
#[tauri::command]
pub async fn reset_app(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    engine.shared().paused_output.store(true, Ordering::Release);

    if let Err(err) = engine.stop_and_wait(Duration::from_secs(2)).await {
        tracing::warn!(
            ?err,
            "stop_and_wait failed during reset; proceeding with wipe anyway"
        );
    }

    // Guard against irreversible data loss (issue #367): snapshot every
    // profile to a wipe-surviving folder BEFORE tearing anything down, so
    // a user who resets can re-import their playlists + listening history
    // (`play_event`) afterwards. Runs while the active pool is still open
    // so the `VACUUM INTO` snapshot sees a consistent, complete `data.db`.
    // Best-effort — it must never block the reset (a backup error is not a
    // reason to trap the user in a broken install).
    if let Some(dir) = safety_backup_all_profiles(&state).await {
        tracing::info!(
            dir = %dir.display(),
            "reset_app: pre-reset safety backup written; re-importable via the profile importer"
        );
    }

    let root = state.paths.root.clone();

    state.deactivate_profile().await;
    state.app_db.close().await;

    let wipe_root = root.clone();
    match tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&wipe_root)).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {}
        Ok(Err(err)) => {
            // Wipe failed (file held by external process, permission
            // denied, …). The app is already half-torn-down at this
            // point — fall through to the restart, which is the only
            // recovery path that doesn't leave the user stuck.
            tracing::error!(
                ?err,
                "remove_dir_all failed during reset; restarting to recover from half-teardown"
            );
        }
        Err(err) => {
            tracing::error!(
                ?err,
                "reset_app spawn_blocking join failed; restarting to recover from half-teardown"
            );
        }
    }

    app.restart();
}

/// Write a complete `.waveflow` archive of every profile to a location
/// that survives the `reset_app` wipe. The configured backup folder
/// defaults to `<data-root>/backups`, which lives *inside* the directory
/// the reset removes — so it can never be the safety target. We use the
/// OS documents dir (`<documents>/WaveFlow Backups`, falling back to the
/// home dir) instead: outside the wiped root and easy for the user to
/// find afterwards.
///
/// Best-effort throughout — a failure on any single profile is logged and
/// skipped, and a total failure returns `None` without aborting the
/// caller. Returns the directory the archives landed in when at least one
/// was written. Guard for issue #367.
async fn safety_backup_all_profiles(state: &AppState) -> Option<PathBuf> {
    let target_dir = dirs::document_dir()
        .or_else(dirs::home_dir)
        .map(|base| base.join("WaveFlow Backups"))?;
    if let Err(err) = std::fs::create_dir_all(&target_dir) {
        tracing::warn!(
            ?err,
            dir = %target_dir.display(),
            "pre-reset safety backup: could not create target dir; skipping"
        );
        return None;
    }

    let profiles: Vec<(i64, String)> =
        match sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM profile ORDER BY id")
            .fetch_all(&state.app_db)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "pre-reset safety backup: could not list profiles; skipping"
                );
                return None;
            }
        };

    let active_id = {
        let guard = state.profile.read().await;
        guard.as_ref().map(|p| p.profile_id)
    };
    let include_meta = read_include_metadata_artwork(&state.app_db)
        .await
        .unwrap_or(true);
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();

    let mut written = 0usize;
    for (profile_id, profile_name) in profiles {
        let profile_dir = state.paths.profile_dir(profile_id);
        let db_path = state.paths.profile_db(profile_id);
        let artwork_dir = state.paths.profile_artwork_dir(profile_id);
        let metadata_artwork_dir = include_meta.then(|| state.paths.metadata_artwork_dir.clone());

        let final_target = target_dir.join(format!(
            "pre-reset-{}-{profile_id}-{stamp}.waveflow",
            sanitize_filename(&profile_name)
        ));
        let tmp_target = final_target.with_extension("part");
        let snapshot_db = final_target.with_extension("dbtmp");

        // 1. Snapshot the db with `VACUUM INTO` instead of raw-copying the
        //    live `data.db`. `reset_app` stops the audio engine but does
        //    not freeze the background analyzer / scanner / sync writers,
        //    and the active pool stays open — so a raw copy could capture
        //    a torn db if a writer commits or a WAL checkpoint fires mid
        //    copy. VACUUM INTO reads a single committed point-in-time view
        //    even under concurrent writes. On failure we skip the profile:
        //    a corrupt archive is a worse safety net than none.
        let snapshot_res = if Some(profile_id) == active_id {
            match state.require_profile_pool().await {
                Ok(pool) => vacuum_into(&pool, &snapshot_db).await,
                Err(err) => Err(err),
            }
        } else {
            vacuum_into_file(&db_path, &snapshot_db).await
        };
        if let Err(err) = snapshot_res {
            tracing::warn!(
                ?err,
                profile_id,
                "pre-reset safety backup: db snapshot failed; skipping this profile"
            );
            let _ = std::fs::remove_file(&snapshot_db);
            continue;
        }

        let manifest = ArchiveManifest {
            archive_version: ARCHIVE_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            profile_name: profile_name.clone(),
            source_profile_id: profile_id,
            exported_at: Utc::now().to_rfc3339(),
        };

        // 2. Archive the SNAPSHOT (not the live db) + artwork into a temp
        //    `.part` sibling, renamed to the final name only on success so
        //    a failed/interrupted write never leaves a truncated file that
        //    looks like a valid backup (same dir → atomic rename).
        let write_res = tokio::task::spawn_blocking({
            let tmp_target = tmp_target.clone();
            let snapshot_db = snapshot_db.clone();
            move || {
                write_archive(
                    &tmp_target,
                    &profile_dir,
                    &snapshot_db,
                    &artwork_dir,
                    metadata_artwork_dir.as_deref(),
                    &manifest,
                )
            }
        })
        .await;
        // The snapshot has been folded into the archive (or the write
        // failed) — drop it either way.
        let _ = std::fs::remove_file(&snapshot_db);

        match write_res {
            Ok(Ok(())) => {
                match std::fs::rename(&tmp_target, &final_target) {
                    Ok(()) => written += 1,
                    Err(err) => {
                        tracing::warn!(?err, profile_id, "pre-reset safety backup: could not finalize archive; discarding partial");
                        let _ = std::fs::remove_file(&tmp_target);
                    }
                }
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    ?err,
                    profile_id,
                    "pre-reset safety backup: archive failed for profile"
                );
                let _ = std::fs::remove_file(&tmp_target);
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    profile_id,
                    "pre-reset safety backup: task join failed for profile"
                );
                let _ = std::fs::remove_file(&tmp_target);
            }
        }
    }

    (written > 0).then_some(target_dir)
}

/// Write a transactionally-consistent snapshot of the database behind
/// `pool` to `snapshot_path` via `VACUUM INTO`. Consistent even while
/// other pool connections keep writing — the snapshot is a single
/// committed point in time — so the subsequent raw copy into the archive
/// can't capture a torn db.
async fn vacuum_into(pool: &sqlx::SqlitePool, snapshot_path: &Path) -> AppResult<()> {
    // VACUUM INTO refuses to overwrite an existing file.
    let _ = std::fs::remove_file(snapshot_path);
    sqlx::query("VACUUM INTO ?")
        .bind(snapshot_path.to_string_lossy().as_ref())
        .execute(pool)
        .await?;
    Ok(())
}

/// Like [`vacuum_into`] for a profile database with no open pool: opens a
/// short-lived connection (safe — an inactive profile holds no other
/// connection) and snapshots it.
async fn vacuum_into_file(db_path: &Path, snapshot_path: &Path) -> AppResult<()> {
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, Connection};

    if !db_path.exists() {
        return Err(AppError::Other(format!(
            "profile database not found: {}",
            db_path.display()
        )));
    }
    let mut conn = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        .connect()
        .await?;
    let _ = std::fs::remove_file(snapshot_path);
    let res = sqlx::query("VACUUM INTO ?")
        .bind(snapshot_path.to_string_lossy().as_ref())
        .execute(&mut conn)
        .await;
    let _ = conn.close().await;
    res?;
    Ok(())
}

/// Strip characters that are hostile in a filename down to `_`, so a
/// profile name like `Léa / Work` yields a portable archive name.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "profile".to_string()
    } else {
        trimmed.to_string()
    }
}

fn regen_in_dir(dir: &Path) -> AppResult<u32> {
    if !dir.exists() {
        return Ok(0);
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(err) => return Err(AppError::Io(err)),
    };

    let mut count: u32 = 0;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(?err, "regen_thumbnails: read_dir entry failed");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.ends_with("_1x") || stem.ends_with("_2x") {
            continue;
        }
        match crate::thumbnails::generate_thumbnails(&path, dir, stem) {
            Ok(()) => {
                count = count.saturating_add(1);
            }
            Err(err) => {
                tracing::warn!(error = %err, %stem, "regen thumbnail failed");
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end check that the `VACUUM INTO ?` bound-parameter form
    /// actually runs, creates the snapshot file, and yields a consistent
    /// copy of the committed data — the crux of the pre-reset safety
    /// backup's snapshot path.
    #[tokio::test]
    async fn vacuum_into_file_snapshots_committed_rows() {
        use sqlx::sqlite::SqliteConnectOptions;
        use sqlx::{ConnectOptions, Connection};

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("data.db");
        let snap_path = dir.path().join("snap.dbtmp");

        {
            let mut conn = SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true)
                .connect()
                .await
                .unwrap();
            sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
                .execute(&mut conn)
                .await
                .unwrap();
            sqlx::query("INSERT INTO t (v) VALUES ('hello')")
                .execute(&mut conn)
                .await
                .unwrap();
            conn.close().await.unwrap();
        }

        vacuum_into_file(&db_path, &snap_path).await.unwrap();
        assert!(snap_path.exists(), "snapshot file should be created");

        let mut snap = SqliteConnectOptions::new()
            .filename(&snap_path)
            .create_if_missing(false)
            .connect()
            .await
            .unwrap();
        let v: String = sqlx::query_scalar("SELECT v FROM t WHERE id = 1")
            .fetch_one(&mut snap)
            .await
            .unwrap();
        assert_eq!(v, "hello");
        snap.close().await.unwrap();

        // VACUUM INTO refuses to overwrite; the helper removes a stale
        // snapshot first, so a second run must also succeed.
        vacuum_into_file(&db_path, &snap_path).await.unwrap();
    }

    /// A missing profile database is a skip (Err), not a silent success —
    /// the caller drops the profile rather than archiving an empty db.
    #[tokio::test]
    async fn vacuum_into_file_missing_db_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        let snap = dir.path().join("snap.dbtmp");
        assert!(vacuum_into_file(&missing, &snap).await.is_err());
    }
}
