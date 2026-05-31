//! Diagnostic Tauri commands for the sync infrastructure shipped in
//! Phase 1.f.desktop.2. The Settings → Diagnostics panel will use
//! [`sync_get_queue_state`] to show the user how many ops are
//! waiting to be sent and what the local Lamport floor + device id
//! are; [`sync_clear_pending`] is the nuclear option for when the
//! queue is wedged.
//!
//! No CRUD enqueue hooks are wired in this PR — see the
//! [`crate::sync`] module docstring for the scope split.

use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
    sync::{device, drain, lamport, mode, queue},
};

#[derive(Debug, Serialize)]
pub struct SyncQueueState {
    /// Stable per-install device id the server pins its UNIQUEs
    /// against. `None` only on a fresh install before the first call
    /// — production code always goes through [`device::ensure`] so
    /// the diagnostic value mirrors what the future drain task will
    /// send.
    pub device_id: Option<String>,
    /// Per-profile Lamport floor. `0` on a fresh profile, otherwise
    /// the value the next outbound op would slot at (= last issued
    /// `+ 1`).
    pub lamport_local_max: i64,
    /// Number of rows currently in the local queue.
    pub pending_count: i64,
    /// Current per-profile sync mode (`"local"` | `"hybrid"`). Falls
    /// back to `"hybrid"` (the default) on a fresh profile with no
    /// stored row.
    pub mode: &'static str,
}

/// Snapshot of the desktop's sync infrastructure for the Settings
/// panel. Does NOT generate a device id if the row hasn't been
/// planted yet — reading-without-side-effects is safer for a
/// diagnostic surface, and the CRUD enqueue hook (1.f.desktop.2b)
/// is the right place to lazy-create on first write.
#[tauri::command]
pub async fn sync_get_queue_state(state: tauri::State<'_, AppState>) -> AppResult<SyncQueueState> {
    let device_id = device::read(&state.app_db).await?;

    let (lamport_local_max, pending_count, sync_mode) = match state.require_profile_pool().await {
        Ok(pool) => (
            lamport::read(&pool).await?,
            queue::count_pending(&pool).await?,
            mode::read(&pool).await?,
        ),
        Err(err) => {
            // No active profile is the only legitimate path here
            // post-bootstrap (we render defaults so the Settings card
            // can still mount). Anything else — a pool init failure,
            // a closed RwLock, etc. — should at minimum land in the
            // tracing sink so an operator can correlate the "0 / 0"
            // surface with the actual cause instead of staring at a
            // silently-empty card.
            tracing::warn!(
                error = %err,
                "sync_get_queue_state: require_profile_pool failed, returning defaults",
            );
            (0, 0, mode::SyncMode::Hybrid)
        }
    };

    Ok(SyncQueueState {
        device_id,
        lamport_local_max,
        pending_count,
        mode: sync_mode.as_str(),
    })
}

/// Drop every queued op. Used by the Settings diagnostic panel when
/// the user wants a clean slate (e.g. after switching servers).
/// Returns the number of rows that were removed so the UI can
/// surface a confirmation toast.
#[tauri::command]
pub async fn sync_clear_pending(state: tauri::State<'_, AppState>) -> AppResult<u64> {
    let pool = state.require_profile_pool().await?;
    queue::clear(&pool).await
}

#[derive(Debug, Deserialize)]
pub struct SetSyncModeRequest {
    /// Canonical lowercase string — must match
    /// [`mode::SyncMode::as_str`] (currently `"local"` or
    /// `"hybrid"`). Anything else fails 400-style with a clear
    /// error so a typoed JSON payload can't silently land an
    /// unknown mode in storage.
    pub mode: String,
}

/// Read the active profile's current sync mode. Returns the canonical
/// string form so the frontend doesn't have to enumerate the variants
/// in two places.
#[tauri::command]
pub async fn sync_get_mode(state: tauri::State<'_, AppState>) -> AppResult<&'static str> {
    let pool = state.require_profile_pool().await?;
    Ok(mode::read(&pool).await?.as_str())
}

/// Persist the active profile's sync mode.
#[tauri::command]
pub async fn sync_set_mode(
    state: tauri::State<'_, AppState>,
    req: SetSyncModeRequest,
) -> AppResult<&'static str> {
    let parsed = match req.mode.trim() {
        "local" => mode::SyncMode::Local,
        "hybrid" => mode::SyncMode::Hybrid,
        other => {
            return Err(AppError::Other(format!(
                "unknown sync mode '{other}', expected 'local' or 'hybrid'",
            )));
        }
    };
    let pool = state.require_profile_pool().await?;
    mode::write(&pool, parsed).await?;
    // Flipping to Hybrid likely means the user wants their pending
    // ops to fly upstream right away — wake the drain task so the
    // first push doesn't wait for the 30 s tick.
    if parsed == mode::SyncMode::Hybrid {
        state.drain.notify();
    }
    Ok(parsed.as_str())
}

/// Force an immediate drain pass — used by the Settings diagnostic
/// "Push now" button so the operator doesn't have to wait for the
/// periodic tick to verify the wiring.
#[tauri::command]
pub async fn sync_drain_now(state: tauri::State<'_, AppState>) -> AppResult<drain::DrainOutcome> {
    drain::drain_once(&state).await
}
