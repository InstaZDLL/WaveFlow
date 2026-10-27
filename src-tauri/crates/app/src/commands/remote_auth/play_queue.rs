//! Playing a server playlist as a queue, and that queue.

use serde::Serialize;

use crate::{error::AppResult, state::AppState};

/// Play a projected remote playlist as a native queue, starting at
/// `start_index`. Fills the in-memory remote queue from the projection so
/// the tracks after it auto-advance, mints a stream ticket for the first,
/// and hands its URL to the engine. Manual next/previous and end-of-track
/// all route back through [`crate::remote::playback`] while the session is
/// active.
#[tauri::command]
pub async fn remote_play_playlist(
    app: tauri::AppHandle,
    playlist_id: String,
    start_index: usize,
) -> AppResult<()> {
    crate::remote::playback::start(&app, &playlist_id, start_index).await
}

/// One row of the live remote play queue.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteQueueRow {
    pub id: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// Server id of the primary artist — lets the "About the artist" panel
    /// link to the remote artist and fetch its photo for a remote track.
    pub artist_id: Option<String>,
    pub artwork_hash: Option<String>,
    pub duration_ms: Option<i64>,
}

/// The live remote play queue and its cursor, for the queue panel.
#[derive(Debug, Clone, Serialize)]
pub struct RemotePlayQueue {
    pub entries: Vec<RemoteQueueRow>,
    pub index: usize,
}

/// Snapshot the active remote play queue, or `None` when the current
/// playback is a library track or a radio stream. Read from memory —
/// instant, no server round-trip.
#[tauri::command]
pub async fn remote_get_play_queue(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<RemotePlayQueue>> {
    Ok(state
        .remote_playback
        .snapshot()
        .map(|(entries, index)| RemotePlayQueue {
            entries: entries
                .into_iter()
                .map(|e| RemoteQueueRow {
                    id: e.id,
                    title: e.title,
                    artist: e.artist,
                    artist_id: e.artist_id,
                    artwork_hash: e.artwork_hash,
                    duration_ms: e.duration_ms,
                })
                .collect(),
            index,
        }))
}

/// Jump the remote play queue to an absolute position and play it. Backs
/// the queue panel's click-to-jump; a no-op when no remote session is
/// active.
#[tauri::command]
pub async fn remote_queue_jump(app: tauri::AppHandle, index: usize) -> AppResult<()> {
    crate::remote::playback::jump_to(&app, index).await
}
