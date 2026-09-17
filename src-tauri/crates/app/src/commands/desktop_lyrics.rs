//! IPC surface of the floating desktop lyrics window (issue #582). The
//! window logic lives in [`crate::desktop_lyrics`], shared with the tray.

use tauri::AppHandle;

use crate::desktop_lyrics::{self, DesktopLyricsStatus};
use crate::error::AppResult;

#[tauri::command]
pub fn desktop_lyrics_status(app: AppHandle) -> DesktopLyricsStatus {
    desktop_lyrics::status(&app)
}

// Async on purpose: creating a window from a synchronous command
// deadlocks on Windows, and closing one goes through the same event loop.
#[tauri::command]
pub async fn open_desktop_lyrics(app: AppHandle) -> AppResult<()> {
    desktop_lyrics::open(&app).await?;
    Ok(())
}

#[tauri::command]
pub async fn close_desktop_lyrics(app: AppHandle) -> AppResult<()> {
    desktop_lyrics::close(&app)?;
    Ok(())
}

#[tauri::command]
pub async fn set_desktop_lyrics_locked(app: AppHandle, locked: bool) -> AppResult<()> {
    desktop_lyrics::set_locked(&app, locked)?;
    Ok(())
}
