//! Files played without importing them, and the queue they form.

use serde::Serialize;
use tauri::AppHandle;

use crate::{error::AppResult, state::AppState};

/// Play files or folders without importing them, or as library tracks when
/// every file is one ([`crate::external_playback`]).
#[tauri::command]
pub async fn player_play_paths(app: AppHandle, paths: Vec<String>) -> AppResult<()> {
    crate::external_playback::play_paths(
        &app,
        paths.into_iter().map(std::path::PathBuf::from).collect(),
    )
    .await
}

/// One file of the queue of files played without importing them, in the
/// shape of a remote queue row so the queue views render either.
#[derive(Debug, Serialize)]
pub struct ExternalQueueEntry {
    /// The file's path.
    pub id: String,
    /// The file's name without its extension: its tags are read only when
    /// it plays.
    pub title: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub artwork_hash: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ExternalQueuePayload {
    pub entries: Vec<ExternalQueueEntry>,
    pub index: usize,
}

/// The queue of files played without importing them, for the queue panel;
/// `None` when no such queue is playing.
#[tauri::command]
pub fn player_get_external_queue(
    state: tauri::State<'_, AppState>,
) -> Option<ExternalQueuePayload> {
    let (entries, index) = state.external_playback.snapshot()?;
    Some(ExternalQueuePayload {
        entries: entries
            .into_iter()
            .map(|entry| ExternalQueueEntry {
                title: entry
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                id: entry.path.display().to_string(),
                artist: None,
                artist_id: None,
                artwork_hash: None,
                duration_ms: None,
            })
            .collect(),
        index,
    })
}

/// Jump to a file of that queue and play it.
#[tauri::command]
pub async fn player_external_jump(app: AppHandle, index: usize) -> AppResult<()> {
    crate::external_playback::jump_to(&app, index).await
}
