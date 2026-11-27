//! WaveSync (RFC-010): thin wrappers over [`crate::wavesync::WaveSync`].

use tauri::AppHandle;

use crate::error::AppResult;
use crate::state::AppState;
use crate::wavesync::{AddOutcome, Discovered, EndReason, View, WaveSync};

#[tauri::command]
pub fn wavesync_get_view(ws: tauri::State<'_, WaveSync>) -> View {
    ws.view()
}

#[tauri::command]
pub async fn wavesync_start(
    app: AppHandle,
    ws: tauri::State<'_, WaveSync>,
    name: String,
    display_name: String,
    approval: bool,
) -> AppResult<View> {
    ws.start(&app, &name, &display_name, approval).await
}

#[tauri::command]
pub async fn wavesync_discover(ws: tauri::State<'_, WaveSync>) -> AppResult<Vec<Discovered>> {
    ws.discover().await
}

#[tauri::command]
pub async fn wavesync_join(
    app: AppHandle,
    ws: tauri::State<'_, WaveSync>,
    target: String,
    fingerprint: Option<String>,
    code: Option<String>,
    display_name: String,
) -> AppResult<View> {
    ws.join(
        &app,
        &target,
        fingerprint.as_deref(),
        code.as_deref(),
        &display_name,
    )
    .await
}

/// Leave the session joined, or end the one hosted.
#[tauri::command]
pub fn wavesync_leave(app: AppHandle, ws: tauri::State<'_, WaveSync>) {
    let reason = if ws.view().role == Some("host") {
        EndReason::Ended
    } else {
        EndReason::Left
    };
    ws.end(&app, reason);
}

#[tauri::command]
pub fn wavesync_dismiss(app: AppHandle, ws: tauri::State<'_, WaveSync>) {
    ws.dismiss(&app);
}

#[tauri::command]
pub fn wavesync_approve(
    app: AppHandle,
    ws: tauri::State<'_, WaveSync>,
    request: u32,
    accept: bool,
) -> AppResult<()> {
    ws.approve(&app, request, accept)
}

#[tauri::command]
pub fn wavesync_new_code(app: AppHandle, ws: tauri::State<'_, WaveSync>) -> AppResult<()> {
    ws.new_code(&app)
}

#[tauri::command]
pub fn wavesync_remove_participant(
    app: AppHandle,
    ws: tauri::State<'_, WaveSync>,
    participant: u32,
) -> AppResult<()> {
    ws.remove_participant(&app, participant)
}

#[tauri::command]
pub async fn wavesync_add(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    ws: tauri::State<'_, WaveSync>,
    track_ids: Vec<i64>,
) -> AppResult<AddOutcome> {
    ws.add(&app, &state, &track_ids).await
}

#[tauri::command]
pub async fn wavesync_remove(
    app: AppHandle,
    ws: tauri::State<'_, WaveSync>,
    entry: String,
) -> AppResult<()> {
    ws.remove(&app, &entry).await
}
