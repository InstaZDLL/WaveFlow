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

use waveflow_core::metadata::lastfm::{LastfmClient, LastfmError};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

const LASTFM_KEY: &str = "app.lastfm_api_key";
const LASTFM_SECRET: &str = "app.lastfm_api_secret";
/// Discord Rich Presence opt-in flag. Stored in `app_setting` (shared
/// across profiles) because users expect the toggle to follow them
/// across profiles — it's a privacy-style preference, not a per-
/// account integration like Last.fm scrobbling.
const DISCORD_RPC_KEY: &str = "integrations.discord_rpc";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Return the stored Last.fm API key, or `None` if never configured
/// (or cleared by the user).
#[tauri::command]
pub async fn get_lastfm_api_key(state: tauri::State<'_, AppState>) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
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
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(LASTFM_KEY)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}

/// Internal helper to read the matching shared secret. Both must be
/// present for any signed call to succeed.
pub async fn read_lastfm_api_secret(state: &AppState) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(LASTFM_SECRET)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}

// ── Artist bio source preference (issue #295) ──────────────────────
//
// `metadata.bio_source` selects which provider fills the artist bio:
// Last.fm (default — English only, needs the user's API key) or
// TheAudioDB (multi-language, no key). `metadata.bio_language` is the
// TheAudioDB language code (ignored for Last.fm). Both are app-wide
// like the Last.fm key — bios live in the shared `metadata_artist`
// cache, keyed by deezer_id.
const BIO_SOURCE_KEY: &str = "metadata.bio_source";
const BIO_LANGUAGE_KEY: &str = "metadata.bio_language";
const DEFAULT_BIO_LANGUAGE: &str = "en";
/// Language codes TheAudioDB ships a biography for (others fall back to
/// English inside the client). Also gates `set_bio_language` writes.
pub const BIO_LANGUAGES: &[&str] = &["en", "fr", "de", "es", "it", "pt", "nl", "ru", "ja", "zh"];

/// Provider used to fill artist biographies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BioSource {
    Lastfm,
    TheAudioDb,
}

impl BioSource {
    /// Parse a persisted / IPC value; anything but `"theaudiodb"` is
    /// Last.fm so a stray value can't silently disable bios.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("theaudiodb") => BioSource::TheAudioDb,
            _ => BioSource::Lastfm,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            BioSource::Lastfm => "lastfm",
            BioSource::TheAudioDb => "theaudiodb",
        }
    }
}

#[tauri::command]
pub async fn get_bio_source(state: tauri::State<'_, AppState>) -> AppResult<String> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(BIO_SOURCE_KEY)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(BioSource::parse(value.as_deref()).as_str().to_string())
}

#[tauri::command]
pub async fn set_bio_source(state: tauri::State<'_, AppState>, source: String) -> AppResult<()> {
    let normalized = BioSource::parse(Some(source.as_str())).as_str();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(BIO_SOURCE_KEY)
    .bind(normalized)
    .bind(now_ms())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Internal helper: the active bio provider (defaults to Last.fm).
pub async fn read_bio_source(state: &AppState) -> AppResult<BioSource> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(BIO_SOURCE_KEY)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(BioSource::parse(value.as_deref()))
}

#[tauri::command]
pub async fn get_bio_language(state: tauri::State<'_, AppState>) -> AppResult<String> {
    read_bio_language(&state).await
}

#[tauri::command]
pub async fn set_bio_language(
    state: tauri::State<'_, AppState>,
    language: String,
) -> AppResult<()> {
    // Clamp to a known TheAudioDB language so an arbitrary write can't
    // strand the cache on a code the client never resolves.
    let lang = if BIO_LANGUAGES.contains(&language.as_str()) {
        language.as_str()
    } else {
        DEFAULT_BIO_LANGUAGE
    };
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(BIO_LANGUAGE_KEY)
    .bind(lang)
    .bind(now_ms())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Internal helper: TheAudioDB bio language (defaults to English).
pub async fn read_bio_language(state: &AppState) -> AppResult<String> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(BIO_LANGUAGE_KEY)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(value
        .filter(|v| BIO_LANGUAGES.contains(&v.as_str()))
        .unwrap_or_else(|| DEFAULT_BIO_LANGUAGE.to_string()))
}

/// Return the stored Last.fm API secret, mirrors [`get_lastfm_api_key`].
#[tauri::command]
pub async fn get_lastfm_api_secret(state: tauri::State<'_, AppState>) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
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
pub async fn lastfm_get_status(state: tauri::State<'_, AppState>) -> AppResult<LastfmStatus> {
    let api_key = read_lastfm_api_key(&state).await?;
    let api_secret = read_lastfm_api_secret(&state).await?;
    let configured = api_key.is_some() && api_secret.is_some();

    let username = match state.require_profile_pool().await {
        Ok(pool) => sqlx::query_scalar::<_, Option<String>>(
            "SELECT username FROM auth_credential WHERE provider = 'lastfm'",
        )
        .fetch_optional(&pool)
        .await?
        .flatten(),
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
    // Honour the process-wide offline flag (`app_setting['network.offline_mode']`,
    // mirrored in memory by `offline::is_offline()`). A blind network call
    // here would hang the Settings dialog for the full reqwest timeout —
    // surface a clear error instead.
    if crate::offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is enabled — disable it to sign in to Last.fm".into(),
        ));
    }
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

/// Return the current Discord Rich Presence opt-in flag. Defaults
/// to `false` (off) when never configured.
#[tauri::command]
pub async fn get_discord_rpc_enabled(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    Ok(crate::discord_presence::read_enabled(&state.app_db).await)
}

/// Toggle Discord Rich Presence on / off. Persists to `app_setting`
/// and immediately notifies the running presence worker so the
/// activity card appears/disappears without waiting for the next
/// track change.
#[tauri::command]
pub async fn set_discord_rpc_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(DISCORD_RPC_KEY)
    .bind(if enabled { "true" } else { "false" })
    .bind(now_ms())
    .execute(&state.app_db)
    .await?;

    if let Some(presence) =
        tauri::Manager::try_state::<crate::discord_presence::DiscordPresenceHandle>(&app)
    {
        presence.set_enabled(enabled);
    }
    Ok(())
}

/// Return the current track-change notification opt-in flag. Defaults
/// to `false` (off) when never configured — toast notifications are
/// intrusive and we want explicit opt-in rather than silent spam.
#[tauri::command]
pub async fn get_notifications_track_change(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    Ok(crate::notifications::read_enabled(&state.app_db).await)
}

/// Toggle native track-change notifications. Persists to `app_setting`.
/// Takes effect on the **next** track change — we don't fire a toast
/// for the current track because that would be noise immediately after
/// the user flipped the toggle.
#[tauri::command]
pub async fn set_notifications_track_change(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(crate::notifications::TRACK_CHANGE_KEY)
    .bind(if enabled { "true" } else { "false" })
    .bind(now_ms())
    .execute(&state.app_db)
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
