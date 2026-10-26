//! System-wide shortcuts: the Settings card's reads and writes. The
//! registration itself lives in [`crate::global_shortcuts`].

use tauri::AppHandle;

use crate::{
    error::AppResult,
    global_shortcuts::{GlobalAction, GlobalShortcuts, Snapshot},
    state::AppState,
};

/// The backend this session runs on and where each binding stands.
#[tauri::command]
pub async fn global_shortcuts_status(
    shortcuts: tauri::State<'_, GlobalShortcuts>,
) -> AppResult<Snapshot> {
    Ok(shortcuts.snapshot().await)
}

/// Bind `action` to `combo` (`Ctrl+Alt+P`), or unbind it with `null`. A
/// combination the system refuses comes back in `rejected` and is not
/// stored.
#[tauri::command]
pub async fn global_shortcuts_set(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    shortcuts: tauri::State<'_, GlobalShortcuts>,
    action: GlobalAction,
    combo: Option<String>,
) -> AppResult<Snapshot> {
    shortcuts
        .set(&app, &state.app_db, action, combo.as_deref())
        .await
}

/// Set the registrations aside while a combination is being recorded
/// (`true`), or put them back (`false`).
#[tauri::command]
pub async fn global_shortcuts_suspend(
    app: AppHandle,
    shortcuts: tauri::State<'_, GlobalShortcuts>,
    suspended: bool,
) -> AppResult<Snapshot> {
    Ok(shortcuts.suspend(&app, suspended).await)
}

/// Unbind every global shortcut.
#[tauri::command]
pub async fn global_shortcuts_clear(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    shortcuts: tauri::State<'_, GlobalShortcuts>,
) -> AppResult<Snapshot> {
    shortcuts.clear(&app, &state.app_db).await
}
