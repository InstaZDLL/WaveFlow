//! Tauri commands driving the DLNA / UPnP MediaServer.
//!
//! All state mutations go through the `DlnaServer` handle owned by
//! `AppState` so the worker thread is the single source of truth on
//! what's bound and serving. Settings persistence is decoupled — the
//! Settings page can update `app_setting` without restarting the
//! server, and the bootstrap path in `lib.rs` reads the same row
//! at launch.

use crate::{
    dlna::{config::DlnaConfig, DlnaResources, DlnaStatus},
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
        let resources = build_resources(&state).await?;
        state.dlna.start(cfg, resources);
    } else {
        state.dlna.stop();
    }
    Ok(state.dlna.status().await)
}

#[tauri::command]
pub async fn dlna_get_status(state: tauri::State<'_, AppState>) -> AppResult<DlnaStatus> {
    Ok(state.dlna.status().await)
}

/// Snapshot the per-profile pool + artwork dirs into a `DlnaResources`
/// the worker thread can hold across requests. Re-call on every
/// start so a profile switch picks up the new pool.
pub async fn build_resources(state: &AppState) -> AppResult<DlnaResources> {
    // Deliberately unleashed: the worker thread holds this pool across
    // requests for the whole lifetime of the server, so a lease would
    // stall every profile switch until the drain timeout. `build_resources`
    // is re-called on switch instead, which is the intended lifecycle.
    let pool = state.require_profile_pool().await?.into_unleashed();
    let profile_id = state.require_profile_id().await?;
    Ok(DlnaResources {
        pool,
        profile_artwork_dir: state.paths.profile_artwork_dir(profile_id),
        metadata_artwork_dir: state.paths.metadata_artwork_dir.clone(),
    })
}
