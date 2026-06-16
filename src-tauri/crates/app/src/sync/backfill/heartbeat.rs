//! Background heartbeat poll for RFC-003 Phase B backfill.
//!
//! Periodically fires [`super::maybe_auto_backfill`] so a desktop
//! that's been online but idle catches up with the server without
//! the user having to flip a Settings toggle. The cadence is
//! per-profile (`profile_setting['sync.backfill.heartbeat_interval_min']`)
//! and re-read at the top of every tick so a user change applies on
//! the next cycle without restarting the app.
//!
//! ## Why this isn't redundant with the boot-time pass
//!
//! [`crate::lib::run`] already fires one [`super::maybe_auto_backfill`]
//! at startup. The heartbeat covers the long-tail case: the user
//! keeps the app open for hours / days. Without it the only
//! catch-up path is manual button presses or sync-mode flips —
//! perfectly fine for active users, less so for the "I leave it
//! open while I work" usage pattern.
//!
//! ## Why no wake handle
//!
//! Unlike the drain task ([`crate::sync::drain::DrainHandle`]) which
//! has CRUD callers `notify()`ing after every commit, there's no
//! latency target for the heartbeat — the gates in
//! [`super::maybe_auto_backfill`] (offline / `SyncMode::Local` /
//! no JWT) short-circuit cheaply, and the wall-clock cadence is the
//! only signal that matters. A cadence change applies at the next
//! tick boundary, which is bounded by the previous cadence value.
//! Acceptable for a setting users typically pick once.

use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::state::AppState;

use super::{maybe_auto_backfill, read_heartbeat_interval_min, HEARTBEAT_INTERVAL_DEFAULT_MIN};

/// Spawn the heartbeat task on the Tauri-managed tokio runtime.
/// Called once from `lib.rs::run` after [`AppState`] is managed.
///
/// MUST use `tauri::async_runtime::spawn` rather than `tokio::spawn`:
/// Tauri's `setup` callback runs OUTSIDE a tokio reactor, so a bare
/// `tokio::spawn` panics with "no reactor running" — same pattern
/// `sync::drain::spawn` documents at its own spawn site. The async
/// body itself can still call `tokio::time::sleep` because, once
/// spawned on the Tauri runtime, the future executes inside the
/// reactor.
///
/// The task lives for the entire process lifetime; there's no
/// stop signal because the runtime tears it down with the rest
/// of the tokio runtime at shutdown.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // First tick fires after the configured interval — the
        // boot-time pass in `lib.rs::run` covers the immediate
        // catch-up window, so the heartbeat doesn't need to race
        // it.
        loop {
            // Read the cadence under the *current* active profile.
            // A profile switch mid-session reaches this naturally
            // because `require_profile_pool` follows the live
            // RwLock; no separate notify needed.
            let interval_min = {
                let state = app.state::<AppState>();
                match state.require_profile_pool().await {
                    Ok(pool) => read_heartbeat_interval_min(&pool)
                        .await
                        .unwrap_or(HEARTBEAT_INTERVAL_DEFAULT_MIN),
                    Err(_) => HEARTBEAT_INTERVAL_DEFAULT_MIN,
                }
            };

            // `as u64` is sound — the SET command clamps to
            // [15, 1440], so the cast can't underflow.
            tokio::time::sleep(Duration::from_secs((interval_min as u64) * 60)).await;

            let state = app.state::<AppState>();
            if let Err(err) = maybe_auto_backfill(state.inner()).await {
                tracing::warn!(error = %err, "heartbeat auto-backfill failed");
            }
        }
    });
}
