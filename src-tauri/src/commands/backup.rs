//! Tauri commands for the auto-backup feature. Thin wrappers around
//! [`crate::backup`] — keep the heavy lifting in the module so a test
//! harness without Tauri can still exercise the logic.

use crate::{
    backup::{read_config, run_one_backup, write_config, BackupConfig, BackupHandle},
    error::AppResult,
    state::AppState,
};

#[tauri::command]
pub async fn get_backup_config(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> AppResult<BackupConfig> {
    read_config(&state, &app).await
}

#[derive(Debug, serde::Deserialize)]
pub struct BackupConfigInput {
    pub enabled: bool,
    pub interval_days: i64,
    pub folder: String,
    pub retention: i64,
}

#[tauri::command]
pub async fn set_backup_config(
    state: tauri::State<'_, AppState>,
    backup: tauri::State<'_, BackupHandle>,
    input: BackupConfigInput,
) -> AppResult<()> {
    write_config(
        &state,
        &backup,
        input.enabled,
        input.interval_days,
        input.folder,
        input.retention,
    )
    .await
}

/// Manual "Run backup now" trigger. Returns the list of created
/// archive paths so the frontend can show a toast like "3 backups
/// written to <folder>".
#[tauri::command]
pub async fn run_backup_now(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> AppResult<Vec<String>> {
    let config = read_config(&state, &app).await?;
    run_one_backup(&state, &app, &config).await
}
