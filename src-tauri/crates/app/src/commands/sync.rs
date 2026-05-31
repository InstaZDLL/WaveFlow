//! Diagnostic Tauri commands for the sync infrastructure shipped in
//! Phase 1.f.desktop.2. The Settings → Diagnostics panel will use
//! [`sync_get_queue_state`] to show the user how many ops are
//! waiting to be sent and what the local Lamport floor + device id
//! are; [`sync_clear_pending`] is the nuclear option for when the
//! queue is wedged.
//!
//! No CRUD enqueue hooks are wired in this PR — see the
//! [`crate::sync`] module docstring for the scope split.

use serde::Serialize;

use crate::{
    error::AppResult,
    state::AppState,
    sync::{device, lamport, queue},
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
}

/// Snapshot of the desktop's sync infrastructure for the Settings
/// panel. Does NOT generate a device id if the row hasn't been
/// planted yet — reading-without-side-effects is safer for a
/// diagnostic surface, and the CRUD enqueue hook (1.f.desktop.2b)
/// is the right place to lazy-create on first write.
#[tauri::command]
pub async fn sync_get_queue_state(state: tauri::State<'_, AppState>) -> AppResult<SyncQueueState> {
    let device_id = device::read(&state.app_db).await?;

    let (lamport_local_max, pending_count) = match state.require_profile_pool().await {
        Ok(pool) => (
            lamport::read(&pool).await?,
            queue::count_pending(&pool).await?,
        ),
        // No active profile — return defaults so the UI can render
        // without a hard error. Should be unreachable post-bootstrap
        // but covered defensively.
        Err(_) => (0, 0),
    };

    Ok(SyncQueueState {
        device_id,
        lamport_local_max,
        pending_count,
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
