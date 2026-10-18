//! The XWayland choice, as Settings sees it (#894). The decision itself
//! runs before any window exists, in [`crate::display_backend`].

use crate::display_backend::{self, DisplayBackendStatus};
use crate::error::{AppError, AppResult};

/// Whether the choice is offered, what is stored, and what this launch
/// actually runs on.
#[tauri::command]
pub fn display_backend_status() -> DisplayBackendStatus {
    display_backend::status()
}

/// Store the choice. It applies from the next start: GTK picked its
/// backend when this process began, and nothing can move it now.
#[tauri::command]
pub fn display_backend_set_xwayland(enabled: bool) -> AppResult<DisplayBackendStatus> {
    if !display_backend::status().available {
        // Refused rather than stored: off a Wayland session the choice
        // does nothing, and a card claiming it did would be lying.
        return Err(AppError::Other(
            "XWayland is only a choice on a Linux Wayland session that runs it".into(),
        ));
    }
    display_backend::set_xwayland(enabled)
        .map_err(|err| AppError::Other(format!("could not save the display choice: {err}")))?;
    Ok(display_backend::status())
}
