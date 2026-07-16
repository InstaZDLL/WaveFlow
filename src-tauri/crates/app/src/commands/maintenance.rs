//! Maintenance commands. Bulk operations a user can trigger from the
//! Settings screen (regenerate thumbnails, prune orphan covers, …).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tauri::AppHandle;

use crate::{
    audio::AudioEngine,
    commands::profile_io::{
        checkpoint_wal, read_include_metadata_artwork, write_archive, ArchiveManifest,
        ARCHIVE_VERSION,
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
    // so the WAL checkpoint captures a complete `data.db`. Best-effort —
    // it must never block the reset (a backup error is not a reason to
    // trap the user in a broken install).
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
                tracing::warn!(?err, "pre-reset safety backup: could not list profiles; skipping");
                return None;
            }
        };

    // Fold the active profile's pending WAL pages back into its main file
    // so the copied `data.db` is a complete snapshot. Inactive profiles
    // were already checkpointed when they were deactivated.
    if let Ok(pool) = state.require_profile_pool().await {
        if let Err(err) = checkpoint_wal(&pool).await {
            tracing::warn!(?err, "pre-reset safety backup: WAL checkpoint failed; snapshot may miss the newest writes");
        }
    }

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

        let manifest = ArchiveManifest {
            archive_version: ARCHIVE_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            profile_name: profile_name.clone(),
            source_profile_id: profile_id,
            exported_at: Utc::now().to_rfc3339(),
        };

        let target = target_dir.join(format!(
            "pre-reset-{}-{profile_id}-{stamp}.waveflow",
            sanitize_filename(&profile_name)
        ));

        let res = tokio::task::spawn_blocking(move || {
            write_archive(
                &target,
                &profile_dir,
                &db_path,
                &artwork_dir,
                metadata_artwork_dir.as_deref(),
                &manifest,
            )
        })
        .await;
        match res {
            Ok(Ok(())) => written += 1,
            Ok(Err(err)) => {
                tracing::warn!(?err, profile_id, "pre-reset safety backup: archive failed for profile")
            }
            Err(err) => {
                tracing::warn!(?err, profile_id, "pre-reset safety backup: task join failed for profile")
            }
        }
    }

    (written > 0).then_some(target_dir)
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
