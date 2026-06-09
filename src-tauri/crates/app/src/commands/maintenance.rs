//! Maintenance commands. Bulk operations a user can trigger from the
//! Settings screen (regenerate thumbnails, prune orphan covers, …).

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::AppHandle;

use crate::{
    audio::AudioEngine,
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
    engine
        .shared()
        .paused_output
        .store(true, Ordering::Release);

    if let Err(err) = engine.stop_and_wait(Duration::from_secs(2)).await {
        tracing::warn!(
            ?err,
            "stop_and_wait failed during reset; proceeding with wipe anyway"
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
