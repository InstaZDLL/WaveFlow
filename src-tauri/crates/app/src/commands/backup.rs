//! Tauri commands for the auto-backup feature. Thin wrappers around
//! [`crate::backup`] — keep the heavy lifting in the module so a test
//! harness without Tauri can still exercise the logic.

use crate::{
    backup::{read_config, run_one_backup, write_config, BackupConfig, BackupHandle, BackupPass},
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
    pub include_metadata_artwork: bool,
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
        input.include_metadata_artwork,
    )
    .await
}

/// Manual "Run backup now" trigger. Returns the created archive paths
/// so the frontend can show a toast like "3 backups written", and
/// whether the user stopped the pass.
///
/// Both, because the paths alone cannot be read. An empty list means
/// either "you stopped it before the first archive finished" or "every
/// profile failed" -- `run_one_backup` logs a failing profile and
/// carries on, by design, so a pass where all of them fail returns
/// exactly what a cancelled one does. Saying nothing in both cases
/// leaves a real failure silent; saying "0 archives written" in both
/// answers a deliberate stop with a result. The flag is what tells
/// them apart.
#[tauri::command]
pub async fn run_backup_now(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> AppResult<BackupPass> {
    let config = read_config(&state, &app).await?;
    run_one_backup(&state, &app, &config).await
}
