//! The XWayland choice, as Settings sees it (#894). The decision itself
//! runs before any window exists, in [`crate::display_backend`].

use tauri::AppHandle;

use crate::display_backend::{self, DisplayBackendStatus};
use crate::error::{AppError, AppResult};

/// Whether the main window was opened on X11 (XWayland), read from its
/// native handle rather than from the environment that asked for it.
/// `None` off Linux, or when there is no window to ask yet.
fn opened_on_x11(app: &AppHandle) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use tauri::Manager as _;
        let window = app.get_webview_window("main")?;
        let handle = window.window_handle().ok()?;
        Some(matches!(
            handle.as_raw(),
            RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_)
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        None
    }
}

/// Whether the choice is offered, what is stored, and what this launch
/// actually runs on.
#[tauri::command]
pub fn display_backend_status(app: AppHandle) -> DisplayBackendStatus {
    display_backend::status(opened_on_x11(&app))
}

/// Store the choice. It applies from the next start: GTK picked its
/// backend when this process began, and nothing can move it now.
#[tauri::command]
pub fn display_backend_set_xwayland(
    app: AppHandle,
    enabled: bool,
) -> AppResult<DisplayBackendStatus> {
    if !display_backend::status(None).available {
        // Refused rather than stored: off a Wayland session the choice
        // does nothing, and a card claiming it did would be lying.
        return Err(AppError::Other(
            "XWayland is only a choice on a Linux Wayland session that runs it".into(),
        ));
    }
    display_backend::set_xwayland(enabled)
        .map_err(|err| AppError::Other(format!("could not save the display choice: {err}")))?;
    Ok(display_backend::status(opened_on_x11(&app)))
}
