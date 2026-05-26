//! Generic "save share image" sink — accepts a raw PNG byte stream
//! from the frontend Canvas renderer and writes it to the user-picked
//! path. Shared by Wrapped PNG export and Now-Playing card export so
//! we don't reimplement the IPC byte channel + spawn_blocking write
//! per feature.

use crate::error::AppResult;

/// Persist a frontend-rendered PNG at the chosen path. The bytes flow
/// through the IPC channel as `Vec<u8>` (numeric JSON array on the
/// wire) rather than as a base64 data-URL because IPC strings are
/// UTF-16 in WebView2 and a 1080×1920 PNG roughly doubles in memory
/// after base64 — for clip-bound writes the binary detour is worth it.
///
/// File I/O runs on `spawn_blocking` so a slow disk (USB drive,
/// network share) can't stall the tokio runtime.
#[tauri::command]
pub async fn save_share_image(bytes: Vec<u8>, target_path: String) -> AppResult<()> {
    let target = std::path::PathBuf::from(target_path);
    tokio::task::spawn_blocking(move || std::fs::write(&target, bytes))
        .await
        .map_err(|e| crate::error::AppError::Other(format!("share image task: {e}")))?
        .map_err(|e| crate::error::AppError::Other(format!("share image write: {e}")))?;
    Ok(())
}
