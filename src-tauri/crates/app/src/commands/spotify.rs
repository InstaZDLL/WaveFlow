//! Tauri commands for Spotify OAuth and Web API reads.
//!
//! Thin wrappers over the `waveflow-spotify` crate. The app crate owns
//! `AppState` → pool unwrapping; the Spotify crate stays ignorant of
//! Tauri / `AppState` so it can be reused outside the desktop.

use std::time::Duration;

use serde::Deserialize;
use tauri::Manager;
use tiny_http::Server;
use waveflow_spotify as spotify;
use waveflow_spotify::{
    SpotifyAccessToken, SpotifyPlaylistLite, SpotifySearchResults, SpotifyStatus, SpotifyTrackLite,
    CALLBACK_ADDR, CALLBACK_URL, SCOPES,
};

use crate::{
    audio::{engine::AudioCmd, AudioEngine},
    commands::loopback::html_response,
    error::{AppError, AppResult},
    offline,
    state::AppState,
};

/// Error message returned by every Spotify command that would touch
/// the network while [`offline`] mode is on. Mirrors the Deezer /
/// Last.fm / LRCLIB pattern — each outbound HTTP path gates on
/// `is_offline()` per the CLAUDE.md cross-cutting rule. Caller
/// surfaces this directly in toast / dialog UI, so the message is
/// part of the contract.
const OFFLINE_ERR: &str = "Spotify is unavailable while offline mode is on";

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[tauri::command]
pub async fn get_spotify_client_id(state: tauri::State<'_, AppState>) -> AppResult<Option<String>> {
    Ok(spotify::read_client_id(&state.app_db).await?)
}

#[tauri::command]
pub async fn set_spotify_client_id(
    state: tauri::State<'_, AppState>,
    client_id: String,
) -> AppResult<()> {
    spotify::write_client_id(&state.app_db, &client_id).await?;
    Ok(())
}

#[tauri::command]
pub async fn spotify_get_status(state: tauri::State<'_, AppState>) -> AppResult<SpotifyStatus> {
    // The Spotify crate accepts `Option<&SqlitePool>` for the profile
    // pool so it can still report `configured` when no profile is
    // mounted. Match on the specific `NoActiveProfile` variant —
    // a bare `.ok()` would swallow real DB / pool errors (e.g.
    // SQLite file deleted under our feet) and silently report
    // `connected: false`, which makes diagnosing a broken install
    // harder. Anything else propagates through `?`.
    let profile_pool = match state.require_profile_pool().await {
        Ok(pool) => Some(pool),
        Err(AppError::NoActiveProfile) => None,
        Err(err) => return Err(err),
    };
    Ok(spotify::status(&state.app_db, profile_pool.as_ref()).await?)
}

#[tauri::command]
pub async fn spotify_logout(state: tauri::State<'_, AppState>) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    sqlx::query("DELETE FROM auth_credential WHERE provider = 'spotify'")
        .execute(&pool)
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn spotify_login(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<SpotifyStatus> {
    if offline::is_offline() {
        return Err(AppError::Other(OFFLINE_ERR.into()));
    }
    let client_id = spotify::read_client_id(&state.app_db)
        .await?
        .ok_or_else(|| AppError::Other("Spotify Client ID is not configured".into()))?;
    let verifier = spotify::random_token();
    let challenge = spotify::pkce_challenge(&verifier);
    let csrf_state = spotify::random_token();
    let auth_url = format!(
        "https://accounts.spotify.com/authorize?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", CALLBACK_URL)
            .append_pair("scope", SCOPES)
            .append_pair("state", &csrf_state)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge)
            .finish()
    );

    let expected_state = csrf_state.clone();
    let callback = tauri::async_runtime::spawn_blocking(move || wait_for_callback(&expected_state));

    tauri_plugin_opener::open_url(auth_url, None::<&str>)
        .map_err(|err| AppError::Other(format!("open Spotify login: {err}")))?;

    let code = callback
        .await
        .map_err(|e| AppError::Other(format!("Spotify callback task failed: {e}")))??;

    let client = reqwest::Client::new();
    let tokens = spotify::exchange_code(&client, &client_id, &code, &verifier).await?;
    let (username, _product) = spotify::me(&client, &tokens.access_token).await?;
    let profile_pool = state.require_profile_pool().await?;
    spotify::store_tokens(&profile_pool, &tokens, &username, None).await?;

    if let Some(engine) = app.try_state::<std::sync::Arc<AudioEngine>>() {
        let _ = engine.send(AudioCmd::Pause);
    }

    Ok(spotify::status(&state.app_db, Some(&profile_pool)).await?)
}

#[tauri::command]
pub async fn spotify_get_access_token(
    state: tauri::State<'_, AppState>,
) -> AppResult<SpotifyAccessToken> {
    // No explicit `is_offline()` guard: `spotify::access_token`
    // returns the cached bearer token unmodified when it's still
    // within `REFRESH_SKEW_MS` of expiry, so a guard at the top
    // would block the legitimate "return cached" path even when no
    // network call is needed. When a refresh IS required and the
    // app is offline, the inner `refresh_token()` reqwest call
    // surfaces a clear network error.
    let profile_pool = state.require_profile_pool().await?;
    Ok(spotify::access_token(&state.app_db, &profile_pool).await?)
}

#[tauri::command]
pub async fn spotify_list_playlists(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<SpotifyPlaylistLite>> {
    if offline::is_offline() {
        return Err(AppError::Other(OFFLINE_ERR.into()));
    }
    let profile_pool = state.require_profile_pool().await?;
    let token = spotify::access_token(&state.app_db, &profile_pool).await?;
    let client = reqwest::Client::new();
    Ok(spotify::list_playlists(&client, &token.access_token).await?)
}

#[tauri::command]
pub async fn spotify_get_playlist_tracks(
    state: tauri::State<'_, AppState>,
    playlist_id: String,
) -> AppResult<Vec<SpotifyTrackLite>> {
    if offline::is_offline() {
        return Err(AppError::Other(OFFLINE_ERR.into()));
    }
    let profile_pool = state.require_profile_pool().await?;
    let token = spotify::access_token(&state.app_db, &profile_pool).await?;
    let client = reqwest::Client::new();
    Ok(spotify::playlist_tracks(&client, &token.access_token, &playlist_id).await?)
}

#[tauri::command]
pub async fn spotify_get_queue(
    state: tauri::State<'_, AppState>,
) -> AppResult<spotify::SpotifyQueueSnapshot> {
    if offline::is_offline() {
        return Err(AppError::Other(OFFLINE_ERR.into()));
    }
    let profile_pool = state.require_profile_pool().await?;
    let token = spotify::access_token(&state.app_db, &profile_pool).await?;
    let client = reqwest::Client::new();
    Ok(spotify::queue(&client, &token.access_token).await?)
}

#[tauri::command]
pub async fn spotify_search(
    state: tauri::State<'_, AppState>,
    query: String,
) -> AppResult<SpotifySearchResults> {
    if query.trim().is_empty() {
        return Ok(SpotifySearchResults {
            tracks: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
        });
    }
    if offline::is_offline() {
        return Err(AppError::Other(OFFLINE_ERR.into()));
    }
    let profile_pool = state.require_profile_pool().await?;
    let token = spotify::access_token(&state.app_db, &profile_pool).await?;
    let client = reqwest::Client::new();
    Ok(spotify::search(&client, &token.access_token, query.trim()).await?)
}

#[tauri::command]
pub async fn spotify_pause_local(app: tauri::AppHandle) -> AppResult<()> {
    if let Some(engine) = app.try_state::<std::sync::Arc<AudioEngine>>() {
        engine
            .send(AudioCmd::Pause)
            .map_err(|e| AppError::Audio(format!("pause local player: {e}")))?;
    }
    Ok(())
}

fn wait_for_callback(expected_state: &str) -> AppResult<String> {
    let server = Server::http(CALLBACK_ADDR)
        .map_err(|e| AppError::Other(format!("Spotify callback bind {CALLBACK_ADDR}: {e}")))?;
    let request = server
        .recv_timeout(Duration::from_secs(180))
        .map_err(|e| AppError::Other(format!("Spotify callback receive failed: {e}")))?
        .ok_or_else(|| AppError::Other("Spotify login timed out".into()))?;

    let url = request.url().to_string();
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let parsed = serde_urlencoded::from_str::<CallbackQuery>(query)
        .map_err(|e| AppError::Other(format!("Spotify callback parse failed: {e}")))?;

    let result = match (parsed.code, parsed.error) {
        (Some(code), _) if parsed.state.as_deref() == Some(expected_state) => {
            let _ = request.respond(html_response(
                "<!doctype html><title>WaveFlow Spotify</title><p>Spotify connected. You can close this tab.</p>",
            ));
            Ok(code)
        }
        (_, Some(err)) => {
            let _ = request.respond(html_response(
                "<!doctype html><title>WaveFlow Spotify</title><p>Spotify login was cancelled or denied.</p>",
            ));
            Err(AppError::Other(format!("Spotify login failed: {err}")))
        }
        _ => {
            let _ = request.respond(html_response(
                "<!doctype html><title>WaveFlow Spotify</title><p>Spotify login failed: invalid state.</p>",
            ));
            Err(AppError::Other("Spotify callback state mismatch".into()))
        }
    };
    result
}
