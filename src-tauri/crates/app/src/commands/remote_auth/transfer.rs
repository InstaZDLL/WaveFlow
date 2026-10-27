//! Moving tracks between the device and the server: downloads, uploads
//! and imports into a library folder.

use tauri::Emitter;

use crate::{error::AppResult, state::AppState};

/// Keep a remote track's original bytes on this machine.
///
/// Answers with the existing copy when there already is one, so asking twice
/// costs nothing. Progress arrives as `remote:download-progress`.
#[tauri::command]
pub async fn remote_download_track(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_id: String,
) -> AppResult<crate::remote::download::DownloadedTrack> {
    crate::remote::download::download(&app, &state, &track_id).await
}

/// The server libraries an upload can land in.
///
/// Whether one accepts uploads is the server's to say, and it says it on the
/// first offer rather than here: a non-member is answered with a 404 on
/// purpose, so a "does this library accept files" probe would be an oracle.
#[tauri::command]
pub async fn remote_upload_libraries(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<crate::remote::upload::UploadLibrary>> {
    let (pool, _) = state.require_profile_snapshot().await?;
    crate::remote::upload::libraries(&pool).await
}

/// Read every unlinked local file once and report what the server is missing.
///
/// The expensive half — it hashes whole files — but paid once: the digests are
/// cached and only a retag invalidates one. Progress arrives as
/// `remote:upload-survey`, and [`remote_cancel_upload`] stops it.
#[tauri::command]
pub async fn remote_upload_survey(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::upload::UploadPlan> {
    crate::remote::upload::survey(&app, &state).await
}

/// Offer local tracks to one server library, one session at a time.
///
/// Progress arrives as `remote:upload-progress`.
#[tauri::command]
pub async fn remote_upload_tracks(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    library_id: String,
    track_ids: Vec<i64>,
) -> AppResult<crate::remote::upload::UploadOutcome> {
    crate::remote::upload::upload(&app, &state, &library_id, &track_ids).await
}

/// Stop a survey or an upload after the track it is on.
///
/// Never mid-track: a committed upload stays committed, and an interrupted
/// one is resumable, so there is nothing here to roll back.
#[tauri::command]
pub async fn remote_cancel_upload() -> AppResult<()> {
    crate::remote::upload::request_cancel();
    Ok(())
}

/// The scanned folders an import can land in.
///
/// A folder whose `exists` is false is still listed: telling someone their
/// external drive is unplugged is more useful than quietly dropping the
/// destination they have always used.
#[tauri::command]
pub async fn remote_import_folders(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<crate::remote::import::ImportFolder>> {
    let (pool, _) = state.require_profile_snapshot().await?;
    crate::remote::import::folders(&pool).await
}

/// Copy server tracks into a scanned folder, index them, and link each one
/// back to the track it came from.
///
/// Progress arrives as `remote:import-progress`; the folder is scanned once at
/// the end, so `scan:progress` fires too.
#[tauri::command]
pub async fn remote_import_tracks(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<String>,
    folder_id: i64,
) -> AppResult<crate::remote::import::ImportOutcome> {
    let outcome = crate::remote::import::import(&app, &state, &track_ids, folder_id).await?;
    if !outcome.imported.is_empty() {
        // The same two follow-ups `scan_folder` does: freshly written track
        // ops ship now rather than at the drain's next idle poll, and the
        // background analyzer picks up what just arrived.
        state.drain.notify();
        crate::commands::analysis::maybe_auto_analyze(&app);
        let _ = app.emit("library:rescanned", ());
    }
    Ok(outcome)
}

/// Every offline copy, newest first.
#[tauri::command]
pub async fn remote_list_downloads(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<crate::remote::download::DownloadedTrack>> {
    let (pool, _) = state.require_profile_snapshot().await?;
    crate::remote::download::list(&pool).await
}

/// Disk held by the managed download folder.
#[tauri::command]
pub async fn remote_downloads_info(
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::download::DownloadsInfo> {
    let (pool, _) = state.require_profile_snapshot().await?;
    crate::remote::download::info(&pool).await
}

/// Drop one offline copy. `false` when there was none to drop.
#[tauri::command]
pub async fn remote_remove_download(
    state: tauri::State<'_, AppState>,
    track_id: String,
) -> AppResult<bool> {
    let (pool, _) = state.require_profile_snapshot().await?;
    let mut conn = pool.acquire().await?;
    crate::remote::download::remove(&mut conn, &track_id).await
}

/// Drop every offline copy.
#[tauri::command]
pub async fn remote_clear_downloads(state: tauri::State<'_, AppState>) -> AppResult<usize> {
    let (pool, _) = state.require_profile_snapshot().await?;
    let mut conn = pool.acquire().await?;
    crate::remote::download::clear_all(&mut conn).await
}
