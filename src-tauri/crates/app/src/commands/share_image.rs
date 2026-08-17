//! Save a frontend-rendered PNG to disk.
//!
//! Generic image sink shared by the Wrapped PNG export and the Now-
//! Playing / immersive share cards, so the IPC byte channel +
//! `spawn_blocking` write aren't reimplemented per feature. Formerly
//! bundled in `commands/share.rs`, which mixed it with the v1 public-
//! share links; those links were removed in the RFC-005 cutover, but
//! this sink is unrelated to sync and stays.

use crate::error::{AppError, AppResult};

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
        .map_err(|e| AppError::Other(format!("share image task: {e}")))?
        .map_err(|e| AppError::Other(format!("share image write: {e}")))?;
    Ok(())
}
