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
/// the worker thread can hold across requests.
///
/// Only called from the boot path and from `dlna_set_config` — notably
/// **not** from `switch_profile`, so a running server keeps serving the
/// profile it was started with until it is stopped and restarted. That
/// gap predates the lease work and is tracked in issue #399.
pub async fn build_resources(state: &AppState) -> AppResult<DlnaResources> {
    // Deliberately unleashed: the worker holds this pool for the life of
    // the server, not for the span of a command, so a lease would stall
    // every profile switch until the drain timeout without making the
    // worker any more correct. It must tolerate `PoolClosed` — which is
    // exactly what it hits today after a switch (see #399).
    let pool = state.require_profile_pool().await?.into_unleashed();
    let profile_id = state.require_profile_id().await?;
    Ok(DlnaResources {
        pool,
        profile_artwork_dir: state.paths.profile_artwork_dir(profile_id),
        metadata_artwork_dir: state.paths.metadata_artwork_dir.clone(),
    })
}
