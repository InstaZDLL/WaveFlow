use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

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

/// The version of the SQLite library the app runs on, as the library
/// reports it, for the About page. The other versions there are read from
/// the lockfiles when the frontend is built; this one is asked of the
/// linked library, so a build that links another SQLite still shows its own.
#[tauri::command]
pub fn get_sqlite_version() -> String {
    // SAFETY: `sqlite3_libversion` takes no argument and returns a pointer to
    // a static, NUL-terminated string that lives as long as the library.
    let version = unsafe { std::ffi::CStr::from_ptr(libsqlite3_sys::sqlite3_libversion()) };
    version.to_string_lossy().into_owned()
}

#[tauri::command]
pub async fn open_data_folder(
    state: tauri::State<'_, AppState>,
    profile_id: Option<i64>,
) -> AppResult<()> {
    let path = match profile_id {
        Some(pid) => state.paths.profile_dir(pid),
        None => state.paths.root.clone(),
    };
    crate::external_open::open_path(path)
}

/// Open a web or mail link with the desktop's handler. Replaces the
/// opener plugin's `openUrl` in the frontend so that an AppImage starts
/// the browser with the host's libraries — see [`crate::external_open`].
/// Anything but `http`, `https` and `mailto` is refused: plugins pass
/// their own links through here.
#[tauri::command]
pub async fn open_external_url(url: String) -> AppResult<()> {
    if !crate::external_open::is_openable_url(&url) {
        return Err(AppError::Other(
            "only http, https and mailto links can be opened".into(),
        ));
    }
    crate::external_open::open_url(&url)
}
