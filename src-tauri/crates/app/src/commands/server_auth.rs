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

use std::time::Duration;

use serde::Deserialize;
use tiny_http::{Response, Server};

use crate::{
    error::{AppError, AppResult},
    server_client::{self, ServerStatus},
    state::AppState,
};

/// Loopback callback the OAuth handshake listens on. Picked outside
/// the Spotify port (`49387`) so the two flows can in principle run
/// simultaneously. Static rather than randomised so the user can
/// pre-allowlist it in firewall rules if they want to.
const CALLBACK_ADDR: &str = "127.0.0.1:49388";
const CALLBACK_URL: &str = "http://127.0.0.1:49388/wf/callback";
/// 3-minute receive timeout — matches the Spotify flow and gives the
/// user comfortable headroom to find their password manager.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    token: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

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

/// Persist the `waveflow-web` URL — the Better Auth frontend the
/// OAuth-loopback handshake opens in the browser. Same validation as
/// `server_set_url`.
#[tauri::command]
pub async fn server_set_web_url(
    state: tauri::State<'_, AppState>,
    url: String,
) -> AppResult<ServerStatus> {
    server_client::write_web_url(&state.app_db, &url).await?;
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

/// Open the persisted server URL in the user's default browser. Kept
/// as a fallback for users who prefer the manual-paste path — the
/// OAuth-loopback command below is the default since 1.f.desktop.1b.
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

/// Run the local-loopback OAuth handshake. Mirrors the Spotify flow:
///
/// 1. Generate a random `state` (anti-replay).
/// 2. Bind a one-shot `tiny_http` listener on
///    [`CALLBACK_ADDR`] in a blocking task with a 3-minute receive
///    timeout.
/// 3. Open the user's default browser at
///    `<web-url>/desktop-login?cb=<callback-url>&state=<state>`. The
///    web side (Phase 1.f.desktop.1b PR on `waveflow-web`) validates
///    the session, mints a JWT via `auth.api.getToken`, and
///    302-redirects the browser to our callback URL with the token
///    in the query string.
/// 4. The callback handler parses `?token=…&state=…`, verifies
///    `state` matches what we generated (rejects mismatches),
///    persists the token via `server_client::write_token`, and
///    replies with a small confirmation HTML page.
///
/// Errors short-circuit with [`AppError::Other`] containing a
/// user-readable reason — the UI surfaces them directly.
#[tauri::command]
pub async fn server_begin_loopback_login(
    state: tauri::State<'_, AppState>,
) -> AppResult<ServerStatus> {
    let web_url = server_client::read_web_url(&state.app_db)
        .await?
        .ok_or_else(|| {
            AppError::Other("web URL is not configured; set it in Settings first".into())
        })?;

    let expected_state = random_state();
    let auth_url = format!(
        "{}/desktop-login?{}",
        web_url.trim_end_matches('/'),
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("cb", CALLBACK_URL)
            .append_pair("state", &expected_state)
            .finish(),
    );

    // Spawn the blocking callback waiter BEFORE opening the browser
    // so a fast redirect doesn't race the listener bind.
    let captured_state = expected_state.clone();
    let callback = tauri::async_runtime::spawn_blocking(move || wait_for_callback(&captured_state));

    tauri_plugin_opener::open_url(auth_url, None::<&str>)
        .map_err(|err| AppError::Other(format!("open browser failed: {err}")))?;

    let token = callback
        .await
        .map_err(|err| AppError::Other(format!("callback task panic: {err}")))??;

    server_client::write_token(&state, &token).await?;
    server_client::status(&state).await
}

/// 256-bit URL-safe random token. Two concatenated UUIDv4s give 256
/// bits of entropy — same pattern `spotify::random_token` uses for
/// the PKCE verifier.
fn random_state() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
    )
}

/// Block until the browser hits the callback URL. Validates the
/// `state` matches, extracts `token`, and renders a confirmation
/// HTML page so the user knows it's safe to close the tab.
fn wait_for_callback(expected_state: &str) -> AppResult<String> {
    let server = Server::http(CALLBACK_ADDR).map_err(|err| {
        AppError::Other(format!(
            "could not bind {CALLBACK_ADDR} for OAuth callback: {err}"
        ))
    })?;

    let request = server
        .recv_timeout(CALLBACK_TIMEOUT)
        .map_err(|err| AppError::Other(format!("OAuth callback receive failed: {err}")))?
        .ok_or_else(|| AppError::Other("sign-in timed out — try again".into()))?;

    let url = request.url().to_string();
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let parsed = serde_urlencoded::from_str::<CallbackQuery>(query)
        .map_err(|err| AppError::Other(format!("callback parse failed: {err}")))?;

    let result = match (parsed.token, parsed.error, parsed.state.as_deref()) {
        // Defensive: require `error` to be absent on the success
        // path. The web side never sends both today, but accepting a
        // token alongside an `error` claim would silently mask a
        // future protocol change — falling through to the error arm
        // below is the safer default.
        (Some(token), None, state_value) if state_value == Some(expected_state) => {
            let _ = request.respond(Response::from_string(
                "<!doctype html><title>WaveFlow</title>\
                 <p>Signed in. You can close this tab and return to WaveFlow.</p>",
            ));
            Ok(token)
        }
        (_, Some(err), _) => {
            let _ = request.respond(Response::from_string(
                "<!doctype html><title>WaveFlow</title>\
                 <p>Sign-in was cancelled or denied.</p>",
            ));
            Err(AppError::Other(format!("sign-in failed: {err}")))
        }
        _ => {
            let _ = request.respond(Response::from_string(
                "<!doctype html><title>WaveFlow</title>\
                 <p>Sign-in failed: state mismatch (possible CSRF). Try again from the desktop.</p>",
            ));
            Err(AppError::Other("OAuth callback state mismatch".into()))
        }
    };

    result
}
