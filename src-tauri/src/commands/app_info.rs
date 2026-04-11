use serde::Serialize;

use crate::{error::AppResult, state::AppState};

/// High-level app info returned to the frontend on startup.
///
/// Lets the UI know the version, the resolved data directory and whether a
/// profile is currently active (so it can either jump to the profile
/// selector or restore the last session).
#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub data_dir: String,
    pub app_db_path: String,
    pub active_profile_id: Option<i64>,
}

#[tauri::command]
pub async fn get_app_info(state: tauri::State<'_, AppState>) -> AppResult<AppInfo> {
    let active_profile_id = {
        let guard = state.profile.read().await;
        guard.as_ref().map(|p| p.profile_id)
    };

    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        data_dir: state.paths.root.display().to_string(),
        app_db_path: state.paths.app_db.display().to_string(),
        active_profile_id,
    })
}
