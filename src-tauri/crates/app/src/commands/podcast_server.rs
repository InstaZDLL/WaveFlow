//! A bound server's podcasts: thin commands over [`crate::remote::podcasts`].
//!
//! The same switch as everything else a podcast does: with `podcasts.enabled` off
//! nothing here contacts the server, and offline mode the same (RFC-009 decisions 1
//! and 11). A refusal reaches the frontend as `podcast: <word>`, the word from a fixed
//! list, and nothing here returns an address.
//!
//! Only [`podcast_server_overview`] has a quiet way out: the page goes on without the
//! section, whatever the reason. What is asked of a server afterwards says why it
//! could not be done.

use tauri::AppHandle;

use crate::error::{AppError, AppResult};
use crate::podcast::server::{self, podcast_refusal, ready_client};
use crate::remote::client::{RemoteClient, RemoteFailure};
use crate::remote::podcasts::{self, failure_word, ServerChannel, ServerOverview};
use crate::state::AppState;

/// A client for a gesture, whose refusals are words.
async fn client(state: &AppState) -> AppResult<RemoteClient<'_>> {
    Ok(ready_client(state)
        .await?
        .ok_or_else(|| podcast_refusal("server_signed_out"))?
        .0)
}

fn refused(failure: RemoteFailure) -> AppError {
    // Its status, never its message, which is the server's own text.
    tracing::debug!(status = ?failure.status, "podcast: the server refused a request");
    podcast_refusal(&failure_word(&failure))
}

/// The server's channels, when its podcasts are on and it answered. `None` otherwise,
/// for any reason: switched off here, offline, no server, signed out, off over there,
/// unreachable. The section is simply absent.
#[tauri::command]
pub async fn podcast_server_overview(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<ServerOverview>> {
    let Ok(Some((client, _))) = ready_client(&state).await else {
        return Ok(None);
    };
    Ok(podcasts::visible(podcasts::overview(&client).await))
}

/// One channel and its episodes, read now.
#[tauri::command]
pub async fn podcast_server_channel(
    state: tauri::State<'_, AppState>,
    channel_id: String,
) -> AppResult<ServerChannel> {
    let client = client(&state).await?;
    podcasts::channel(&client, &channel_id)
        .await
        .map_err(|failure| match failure.status {
            Some(404) => podcast_refusal("channel_gone"),
            _ => refused(failure),
        })
}

/// Plays a stored episode from where this account stopped, with its channel's stored
/// episodes as the queue. Refused, and nothing changes, for one that is not stored.
#[tauri::command]
pub async fn podcast_server_play(
    app: AppHandle,
    channel_id: String,
    episode_id: String,
) -> AppResult<()> {
    server::play_episode(&app, &channel_id, &episode_id).await
}

/// Forgets where this account stopped in an episode.
#[tauri::command]
pub async fn podcast_server_clear_position(app: AppHandle, episode_id: String) -> AppResult<()> {
    server::forget_position(&app, episode_id).await
}

/// Subscribes the server to a feed. For an account that may manage podcasts: the page
/// offers it only then, and the server refuses it otherwise.
#[tauri::command]
pub async fn podcast_server_subscribe(
    state: tauri::State<'_, AppState>,
    feed_url: String,
) -> AppResult<ServerChannel> {
    let client = client(&state).await?;
    podcasts::subscribe(&client, &feed_url)
        .await
        .map_err(refused)
}

/// Asks the server to check every channel, which it does in the background.
#[tauri::command]
pub async fn podcast_server_refresh(state: tauri::State<'_, AppState>) -> AppResult<()> {
    let client = client(&state).await?;
    podcasts::refresh(&client).await.map_err(refused)
}

/// Unsubscribes the server from a channel, for every account on it.
#[tauri::command]
pub async fn podcast_server_unsubscribe(
    state: tauri::State<'_, AppState>,
    channel_id: String,
) -> AppResult<()> {
    let client = client(&state).await?;
    podcasts::delete_channel(&client, &channel_id)
        .await
        .map_err(refused)
}

/// Asks the server to keep an episode. It answers at once, and the episode's status
/// says the rest.
#[tauri::command]
pub async fn podcast_server_request_download(
    state: tauri::State<'_, AppState>,
    episode_id: String,
) -> AppResult<()> {
    let client = client(&state).await?;
    podcasts::request_download(&client, &episode_id)
        .await
        .map_err(refused)
}

/// Removes an episode from the server, its file with it, for every account on it.
#[tauri::command]
pub async fn podcast_server_delete_episode(
    state: tauri::State<'_, AppState>,
    episode_id: String,
) -> AppResult<()> {
    let client = client(&state).await?;
    podcasts::delete_episode(&client, &episode_id)
        .await
        .map_err(refused)
}
