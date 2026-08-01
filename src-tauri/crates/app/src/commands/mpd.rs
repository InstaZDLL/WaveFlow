//! Tauri commands driving the MPD server.
//!
//! Same split as [`crate::commands::dlna`]: all state mutation goes
//! through the [`MpdServer`](crate::mpd::MpdServer) handle owned by
//! `AppState`, so the worker thread stays the single source of truth on
//! what is bound. Settings persistence is decoupled — the Settings page
//! writes `app_setting` and the boot path in `lib.rs` reads the same
//! rows at launch.

use crate::{
    error::AppResult,
    mpd::{config::MpdConfig, MpdStatus},
    state::AppState,
};

#[tauri::command]
pub async fn mpd_get_config(state: tauri::State<'_, AppState>) -> AppResult<MpdConfig> {
    crate::mpd::config::load(&state.app_db).await
}

#[tauri::command]
pub async fn mpd_set_config(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    cfg: MpdConfig,
) -> AppResult<MpdStatus> {
    crate::mpd::config::save(&state.app_db, &cfg).await?;
    if cfg.enabled {
        state.mpd.start(cfg, app);
    } else {
        state.mpd.stop();
    }
    Ok(state.mpd.status().await)
}

#[tauri::command]
pub async fn mpd_get_status(state: tauri::State<'_, AppState>) -> AppResult<MpdStatus> {
    Ok(state.mpd.status().await)
}
