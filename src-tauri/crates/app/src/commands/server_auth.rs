//! Tauri commands for the `waveflow-server` account binding.
//!
//! Phase 1.f.desktop.1 — the foundational surface every later sub-PR
//! builds on. Sync (`1.f.desktop.2`), repo swap (`1.f.desktop.3`), and
//! WebSocket subscription (`1.f.desktop.4`) all read their config
//! through [`crate::server_client`].
//!
//! Sign-in flow today: the user opens the server URL in their
//! browser, signs into Better Auth, copies the JWT it issues, and
//! pastes it into the Settings card. A polished local-loopback OAuth
//! flow (mirroring `commands::spotify`) ships in `1.f.desktop.1b`
//! once the matching `/desktop-login` route lands on `waveflow-web`.

use crate::{
    error::{AppError, AppResult},
    server_client::{self, ServerStatus},
    state::AppState,
};

/// Snapshot of the desktop's server binding for the Settings card.
#[tauri::command]
pub async fn server_get_status(state: tauri::State<'_, AppState>) -> AppResult<ServerStatus> {
    server_client::status(&state).await
}

/// Persist the waveflow-server base URL. Validates the value parses
/// as `http(s)://…`; empty input clears the row so the desktop falls
/// back to local-only mode. App-wide (not per-profile) — see the
/// module docstring on [`crate::server_client`].
#[tauri::command]
pub async fn server_set_url(
    state: tauri::State<'_, AppState>,
    url: String,
) -> AppResult<ServerStatus> {
    server_client::write_url(&state.app_db, &url).await?;
    server_client::status(&state).await
}

/// Persist the Bearer JWT the user pasted in from the browser
/// sign-in. Rejects empty / structurally-bad input — leaving a broken
/// token in place would 401 every sync call. Per-profile.
#[tauri::command]
pub async fn server_set_token(
    state: tauri::State<'_, AppState>,
    token: String,
) -> AppResult<ServerStatus> {
    server_client::write_token(&state, &token).await?;
    server_client::status(&state).await
}

/// Sign out the active profile from waveflow-server. Idempotent — no
/// error when the user wasn't signed in to begin with. Server URL
/// stays in place (so the user can re-paste a fresh token without
/// re-entering the URL).
#[tauri::command]
pub async fn server_sign_out(state: tauri::State<'_, AppState>) -> AppResult<ServerStatus> {
    server_client::clear_token(&state).await?;
    server_client::status(&state).await
}

/// Open the persisted server URL in the user's default browser. The
/// landing page itself is whatever `waveflow-web` serves at the root
/// of the configured URL — typically the sign-in page when no
/// session is present, the dashboard otherwise. We don't append a
/// path because the post-1.f.desktop.1b flow will swap this for the
/// loopback handshake; until then the user navigates by hand.
///
/// Returns 400-style errors (via [`AppError::Other`]) when the URL
/// isn't configured yet, so the UI can pull the user back to the
/// "set URL" step instead of opening a blank tab.
#[tauri::command]
pub async fn server_open_login_browser(state: tauri::State<'_, AppState>) -> AppResult<()> {
    let url = server_client::read_url(&state.app_db)
        .await?
        .ok_or_else(|| {
            AppError::Other("server URL is not configured; set it in Settings first".into())
        })?;
    tauri_plugin_opener::open_url(url, None::<&str>)
        .map_err(|err| AppError::Other(format!("open server login: {err}")))?;
    Ok(())
}
