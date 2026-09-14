//! Commands over the running-task registry (issue #601).
//!
//! Thin on purpose: the registry in [`crate::tasks`] owns the state and
//! the routing, and these two just expose it. The status bar subscribes
//! to `tasks:changed` and calls [`list_tasks`] once at mount — in that
//! order, because Tauri does not replay an event to a listener that
//! registered a moment too late.

use crate::{
    error::AppResult,
    tasks::{TaskRegistry, TaskSnapshot},
};
use std::sync::Arc;

#[tauri::command]
pub async fn list_tasks(
    registry: tauri::State<'_, Arc<TaskRegistry>>,
) -> AppResult<Vec<TaskSnapshot>> {
    Ok(registry.snapshot())
}

/// Ask a task to stop, through whatever mechanism it registered.
///
/// Returns `false` when there was nothing to ask: the task finished
/// between the render and the click, or it is already stopping, or it
/// never offered a way out. All three are ordinary — a cancel button
/// races the task it cancels by construction — so none of them is an
/// error.
#[tauri::command]
pub async fn cancel_task(
    registry: tauri::State<'_, Arc<TaskRegistry>>,
    id: u64,
) -> AppResult<bool> {
    Ok(registry.cancel(id))
}
