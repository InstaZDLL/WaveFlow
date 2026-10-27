//! The mirrored catalogue and the reconciliation of local tracks with the
//! server's: the walk, its stats, the links and the playlist conversion.

use crate::{error::AppResult, state::AppState};

/// Scan the local library for conservative M5 reconciliation matches.
///
/// Only exact, unique full-file BLAKE3 matches are linked automatically.
/// Duplicate groups are returned for explicit confirmation.
#[tauri::command]
pub async fn remote_reconcile_scan(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::reconciliation::ReconciliationReport> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::discover_with_progress(&pool, app).await
}

/// Walk the server's catalogue into the projection so both sources can be
/// browsed from one library.
///
/// Incremental: an album whose track count is unchanged is not fetched. Safe
/// to run repeatedly, and safe to cancel — see
/// [`remote_cancel_catalogue_mirror`].
#[tauri::command]
pub async fn remote_mirror_catalogue(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::mirror::MirrorReport> {
    crate::remote::mirror::mirror_catalogue(&state, app).await
}

/// Ask an in-flight [`remote_mirror_catalogue`] to stop. Returns whether a
/// walk was actually running. Whatever was committed stays: the next walk
/// resumes from what is missing rather than starting over.
#[tauri::command]
pub fn remote_cancel_catalogue_mirror() -> bool {
    crate::remote::mirror::request_cancel()
}

/// What the mirror currently holds, for the settings card.
#[tauri::command]
pub async fn remote_catalogue_stats(
    state: tauri::State<'_, AppState>,
) -> AppResult<crate::remote::mirror::CatalogueStats> {
    let pool = state.require_profile_pool().await?;
    crate::remote::mirror::stats(&pool).await
}

/// Drop the mirrored catalogue, keeping every row the user data still needs.
///
/// Refused while a walk owns the slot: the two write the same rows, and
/// interleaving them leaves albums deleted with their tracks still flagged.
#[tauri::command]
pub async fn remote_clear_catalogue(state: tauri::State<'_, AppState>) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    crate::remote::mirror::clear(&pool).await
}

/// Ask an in-flight [`remote_reconcile_scan`] to stop. Returns whether a scan
/// was actually running; a stray click before/after a scan is a no-op.
#[tauri::command]
pub fn remote_cancel_reconcile_scan() -> bool {
    crate::remote::reconciliation::request_cancel()
}

#[tauri::command]
pub async fn remote_list_reconciliation_links(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<crate::remote::reconciliation::ReconciliationLink>> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::links(&pool).await
}

#[tauri::command]
pub async fn remote_confirm_reconciliation(
    state: tauri::State<'_, AppState>,
    local_track_id: i64,
    remote_track_id: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::confirm_exact(&pool, local_track_id, &remote_track_id).await
}

#[tauri::command]
pub async fn remote_reject_reconciliation(
    state: tauri::State<'_, AppState>,
    local_track_id: i64,
    remote_track_id: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::reject_exact(&pool, local_track_id, &remote_track_id).await
}

#[tauri::command]
pub async fn remote_set_reconciliation_preference(
    state: tauri::State<'_, AppState>,
    local_track_id: i64,
    preference: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::set_playback_preference(&pool, local_track_id, &preference).await
}

#[tauri::command]
pub async fn remote_remove_reconciliation_link(
    state: tauri::State<'_, AppState>,
    local_track_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::remove_link(&pool, local_track_id).await
}

#[tauri::command]
pub async fn remote_copy_reconciliation_favorite(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    local_track_id: i64,
    direction: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::copy_favorite(&pool, local_track_id, &direction).await?;
    if direction == "local_to_server" {
        crate::remote::drain::spawn(app);
    }
    Ok(())
}

#[tauri::command]
pub async fn remote_copy_reconciliation_rating(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    local_track_id: i64,
    direction: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::copy_rating(&pool, local_track_id, &direction).await?;
    if direction == "local_to_server" {
        crate::remote::drain::spawn(app);
    }
    Ok(())
}

/// Preview an explicit playlist conversion. No rows are written; every source
/// position is returned with its reconciliation status so the UI can require a
/// deliberate confirmation.
#[tauri::command]
pub async fn remote_preview_playlist_conversion(
    state: tauri::State<'_, AppState>,
    direction: String,
    source_id: String,
) -> AppResult<crate::remote::reconciliation::PlaylistConversionPreview> {
    let pool = state.require_profile_pool().await?;
    crate::remote::reconciliation::preview_playlist_conversion(&pool, &direction, &source_id).await
}

/// Convert a playlist only when every source row still has a confirmed link.
/// The backend rebuilds the preview inside the write transaction so a stale
/// browser confirmation cannot silently omit tracks.
#[tauri::command]
pub async fn remote_convert_playlist(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    direction: String,
    source_id: String,
) -> AppResult<crate::remote::reconciliation::PlaylistConversionResult> {
    let pool = state.require_profile_pool().await?;
    let result =
        crate::remote::reconciliation::convert_playlist(&pool, &direction, &source_id).await?;
    if direction == "local_to_server" {
        crate::remote::drain::spawn(app);
    } else if let Ok(playlist_id) = result.destination_id.parse::<i64>() {
        match state.require_profile_id().await {
            Ok(profile_id) => {
                crate::commands::playlist_cover::maybe_regen_auto_cover(
                    &pool,
                    &state.paths,
                    profile_id,
                    playlist_id,
                )
                .await;
            }
            Err(err) => {
                tracing::warn!(
                    playlist_id,
                    ?err,
                    "playlist conversion succeeded but auto-cover regeneration was skipped"
                );
            }
        }
    }
    Ok(result)
}
