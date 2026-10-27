//! What the account records besides the library: plays, the saved queue
//! and shares.

use crate::{error::AppResult, state::AppState};

/// Record a play against the remote account.
///
/// `submission: false` is a "now playing" ping; only a completed listen
/// enters the local history.
///
/// **The identifier must be the server's.** Nothing calls this from
/// playback, and it should not until remote playback exists: the local
/// player deals in file paths and local row ids, which the server
/// validates and rejects.
#[tauri::command]
pub async fn remote_scrobble(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_id: String,
    submission: bool,
    played_at: Option<i64>,
) -> AppResult<()> {
    crate::remote::write::scrobble(&state, &track_id, submission, played_at).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Save the account's play queue. Same identifier caveat as
/// [`remote_scrobble`].
#[tauri::command]
pub async fn remote_save_queue(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<String>,
    current: Option<String>,
    position_ms: i64,
    client: Option<String>,
) -> AppResult<()> {
    crate::remote::write::save_queue(&state, &track_ids, current, position_ms, client).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Publish a share of remote tracks.
///
/// Returns the local identifier. The public link is not known until the
/// creation reaches the server — the token is derived from a server-side
/// secret — so a share made offline has no link to copy yet.
#[tauri::command]
pub async fn remote_create_share(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<String>,
    description: Option<String>,
    expires_at: Option<i64>,
) -> AppResult<String> {
    let id =
        crate::remote::write::create_share(&state, &track_ids, description, expires_at).await?;
    crate::remote::drain::spawn(app);
    Ok(id)
}

/// Change a share's description or expiry, or empty either.
///
/// The two `clear*` flags are the only way to empty a field: omitting it
/// leaves it in place, so an expiry set by mistake would otherwise be
/// permanent.
#[tauri::command]
pub async fn remote_update_share(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    share_id: String,
    description: Option<String>,
    expires_at: Option<i64>,
    clear_description: Option<bool>,
    clear_expires_at: Option<bool>,
) -> AppResult<()> {
    crate::remote::write::update_share(
        &state,
        &share_id,
        description,
        expires_at,
        clear_description.unwrap_or(false),
        clear_expires_at.unwrap_or(false),
    )
    .await?;
    crate::remote::drain::spawn(app);
    Ok(())
}

/// Withdraw a share.
#[tauri::command]
pub async fn remote_delete_share(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    share_id: String,
) -> AppResult<()> {
    crate::remote::write::delete_share(&state, &share_id).await?;
    crate::remote::drain::spawn(app);
    Ok(())
}
