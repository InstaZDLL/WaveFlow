//! Tauri commands driving the DLNA / UPnP MediaServer.
//!
//! All state mutations go through the `DlnaServer` handle owned by
//! `AppState` so the worker thread is the single source of truth on
//! what's bound and serving. Settings persistence is decoupled — the
//! Settings page can update `app_setting` without restarting the
//! server, and the bootstrap path in `lib.rs` reads the same row
//! at launch.

use crate::{
    dlna::{config::DlnaConfig, DlnaStatus},
    error::AppResult,
    state::AppState,
};

#[tauri::command]
pub async fn dlna_get_config(state: tauri::State<'_, AppState>) -> AppResult<DlnaConfig> {
    crate::dlna::config::load(&state.app_db).await
}

#[tauri::command]
pub async fn dlna_set_config(
    state: tauri::State<'_, AppState>,
    cfg: DlnaConfig,
) -> AppResult<DlnaStatus> {
    crate::dlna::config::save(&state.app_db, &cfg).await?;
    if cfg.enabled {
        state.dlna.start(cfg);
    } else {
        state.dlna.stop();
    }
    Ok(state.dlna.status().await)
}

#[tauri::command]
pub async fn dlna_get_status(state: tauri::State<'_, AppState>) -> AppResult<DlnaStatus> {
    Ok(state.dlna.status().await)
}
