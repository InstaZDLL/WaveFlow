//! Plugin SDK Tauri commands (Phase 3.1 — backend).
//!
//! Surfaces the install-dir contents + per-plugin enabled toggle +
//! uninstall path to the frontend. Install flow lives in a future
//! phase — for v1.5.0 the user sideloads a plugin directory by hand
//! (or it ships pre-installed with a future official channel) and
//! these commands handle everything else.
//!
//! Every command takes the runtime through `AppState::plugins`
//! rather than spinning a new `PluginRuntime` per call — the engine
//! is heavy (cranelift JIT) and reuse keeps the shared `Arc<Engine>`
//! warm across the session.

use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::state::AppState;

mod favorites;
mod info;
mod manage;
mod options;
mod source;
mod ui;

pub use favorites::*;
pub use info::*;
pub use manage::*;
pub use options::*;
pub use source::*;
pub use ui::*;

/// Acquire the per-plugin write lock. Inserts a fresh `Mutex<()>`
/// into the runtime's map the first time we see this id; returns
/// an owned guard the caller holds for the duration of the
/// command. Two commands targeting the same `plugin_id` serialise;
/// commands on different ids run in parallel.
///
/// Hold this BEFORE the manifest-existence check in
/// `set_plugin_enabled` and BEFORE `remove_dir_all` in
/// `uninstall_plugin`. Releasing only at function end (guard
/// drop) keeps the manifest probe + the SQL UPSERT atomic against
/// a racing uninstall.
pub(crate) async fn lock_plugin(state: &AppState, plugin_id: &str) -> OwnedMutexGuard<()> {
    plugin_lock_arc(state, plugin_id).await.lock_owned().await
}

/// Get (or create) the per-plugin lock HANDLE without acquiring it. Only
/// the `plugin_locks` map is briefly locked — the per-plugin mutex itself
/// is left for the caller to acquire. Lets a fan-out grab handles without
/// blocking the loop on a contended plugin, then acquire the guard where it
/// belongs — e.g. `blocking_lock_owned()` inside a `spawn_blocking` task, so
/// the guard spans the actual (uncancellable) work instead of being dropped
/// when an outer async timeout fires.
pub(crate) async fn plugin_lock_arc(state: &AppState, plugin_id: &str) -> Arc<Mutex<()>> {
    let mut map = state.plugin_locks.lock().await;
    map.entry(plugin_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

#[cfg(test)]
mod option_masking_tests;
