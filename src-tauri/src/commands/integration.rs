//! Commands for managing external integration credentials (Last.fm,
//! future: MusicBrainz, etc.) stored in the global `app_setting`
//! table.
//!
//! Keys live in `app_setting` (not per-profile) because an API key is
//! a user-wide concern that shouldn't reset when switching profiles.
//! User-specific session credentials (the result of
//! `auth.getMobileSession`) live per-profile in `auth_credential`
//! since two profiles may scrobble to two different Last.fm accounts.

use chrono::Utc;
use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    lastfm::{LastfmClient, LastfmError},
    state::AppState,
};

const LASTFM_KEY: &str = "app.lastfm_api_key";
const LASTFM_SECRET: &str = "app.lastfm_api_secret";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Return the stored Last.fm API key, or `None` if never configured
/// (or cleared by the user).
#[tauri::command]
pub async fn get_lastfm_api_key(state: tauri::State<'_, AppState>) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(LASTFM_KEY)
    .fetch_optional(&state.app_db)
    .await?;
    Ok(value)
}

/// Upsert the Last.fm API key. Passing an empty string removes the
/// row entirely so the rest of the app treats it as "not configured".
#[tauri::command]
pub async fn set_lastfm_api_key(
    state: tauri::State<'_, AppState>,
    api_key: String,
) -> AppResult<()> {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        sqlx::query("DELETE FROM app_setting WHERE key = ?")
            .bind(LASTFM_KEY)
            .execute(&state.app_db)
            .await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(LASTFM_KEY)
    .bind(trimmed)
    .bind(now_ms())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Internal helper used by `enrich_artist_deezer` to look up the key
/// without having to pass it through Tauri's invoke layer.
pub async fn read_lastfm_api_key(state: &AppState) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(LASTFM_KEY)
    .fetch_optional(&state.app_db)
    .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}

/// Internal helper to read the matching shared secret. Both must be
/// present for any signed call to succeed.
pub async fn read_lastfm_api_secret(state: &AppState) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(LASTFM_SECRET)
    .fetch_optional(&state.app_db)
    .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}

/// Return the stored Last.fm API secret, mirrors [`get_lastfm_api_key`].
#[tauri::command]
pub async fn get_lastfm_api_secret(state: tauri::State<'_, AppState>) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(LASTFM_SECRET)
    .fetch_optional(&state.app_db)
    .await?;
    Ok(value)
}

/// Upsert the Last.fm shared secret. Empty string clears the row.
#[tauri::command]
pub async fn set_lastfm_api_secret(
    state: tauri::State<'_, AppState>,
    api_secret: String,
) -> AppResult<()> {
    let trimmed = api_secret.trim();
    if trimmed.is_empty() {
        sqlx::query("DELETE FROM app_setting WHERE key = ?")
            .bind(LASTFM_SECRET)
            .execute(&state.app_db)
            .await?;
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(LASTFM_SECRET)
    .bind(trimmed)
    .bind(now_ms())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Snapshot of the active profile's Last.fm linkage. `connected` is
/// `true` when an `auth_credential` row for `provider = 'lastfm'`
/// exists; the username is whatever Last.fm canonicalized at login
/// time (e.g. autocorrected from "foo" to "Foo").
#[derive(Debug, Clone, Serialize)]
pub struct LastfmStatus {
    pub configured: bool,
    pub connected: bool,
    pub username: Option<String>,
}

/// Return the current Last.fm linkage state for the active profile.
/// `configured` answers "is the user-supplied API key+secret pair
/// present?", `connected` answers "did we successfully exchange a
/// session key?".
#[tauri::command]
pub async fn lastfm_get_status(
    state: tauri::State<'_, AppState>,
) -> AppResult<LastfmStatus> {
    let api_key = read_lastfm_api_key(&state).await?;
    let api_secret = read_lastfm_api_secret(&state).await?;
    let configured = api_key.is_some() && api_secret.is_some();

    let username = match state.require_profile_pool().await {
        Ok(pool) => {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT username FROM auth_credential WHERE provider = 'lastfm'",
            )
            .fetch_optional(&pool)
            .await?
            .flatten()
        }
        Err(_) => None,
    };
    Ok(LastfmStatus {
        configured,
        connected: username.is_some(),
        username,
    })
}

/// Trade username + password for a session key via Last.fm's
/// `auth.getMobileSession`, then store the resulting key in
/// `auth_credential` so the scrobble worker can pick it up. Errors
/// from Last.fm bubble up so the UI can surface them.
#[tauri::command]
pub async fn lastfm_login(
    state: tauri::State<'_, AppState>,
    username: String,
    password: String,
) -> AppResult<LastfmStatus> {
    let api_key = read_lastfm_api_key(&state)
        .await?
        .ok_or_else(|| AppError::Other("Last.fm API key is not configured".into()))?;
    let api_secret = read_lastfm_api_secret(&state)
        .await?
        .ok_or_else(|| AppError::Other("Last.fm API secret is not configured".into()))?;

    let pool = state.require_profile_pool().await?;
    let client = LastfmClient::new();
    let session = client
        .auth_get_mobile_session(&api_key, &api_secret, &username, &password)
        .await
        .map_err(|e| match e {
            LastfmError::Api { code, message } => {
                AppError::Other(format!("Last.fm error {code}: {message}"))
            }
            other => AppError::Other(other.to_string()),
        })?;

    let now = now_ms();
    sqlx::query(
        "INSERT INTO auth_credential
            (provider, username, token_encrypted, refresh_token_encrypted,
             expires_at, created_at, updated_at)
         VALUES ('lastfm', ?, ?, NULL, NULL, ?, ?)
         ON CONFLICT(provider) DO UPDATE SET
            username = excluded.username,
            token_encrypted = excluded.token_encrypted,
            updated_at = excluded.updated_at",
    )
    .bind(&session.username)
    .bind(session.session_key.as_bytes())
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await?;

    Ok(LastfmStatus {
        configured: true,
        connected: true,
        username: Some(session.username),
    })
}

/// Forget the current profile's Last.fm session. The API key + secret
/// stay in `app_setting` because they're shared across profiles —
/// only the per-profile credential row is cleared.
#[tauri::command]
pub async fn lastfm_logout(state: tauri::State<'_, AppState>) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    sqlx::query("DELETE FROM auth_credential WHERE provider = 'lastfm'")
        .execute(&pool)
        .await?;
    // Drop any pending scrobbles too — they would just fail with an
    // "invalid session" error against a future re-login anyway.
    sqlx::query("DELETE FROM scrobble_queue WHERE provider = 'lastfm'")
        .execute(&pool)
        .await?;
    Ok(())
}

/// Look up the active profile's Last.fm session key, if any. Returns
/// `(api_key, api_secret, session_key, username)` so the worker has
/// every bit it needs to sign and post a scrobble in a single hop.
pub async fn read_lastfm_credentials(
    state: &AppState,
) -> AppResult<Option<(String, String, String, String)>> {
    let api_key = read_lastfm_api_key(state).await?;
    let api_secret = read_lastfm_api_secret(state).await?;
    let (Some(api_key), Some(api_secret)) = (api_key, api_secret) else {
        return Ok(None);
    };
    let pool = match state.require_profile_pool().await {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let row: Option<(Option<String>, Vec<u8>)> = sqlx::query_as(
        "SELECT username, token_encrypted FROM auth_credential WHERE provider = 'lastfm'",
    )
    .fetch_optional(&pool)
    .await?;
    let Some((username, token_bytes)) = row else {
        return Ok(None);
    };
    let session_key = match String::from_utf8(token_bytes) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let username = username.unwrap_or_default();
    if session_key.is_empty() {
        return Ok(None);
    }
    Ok(Some((api_key, api_secret, session_key, username)))
}
