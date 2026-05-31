//! waveflow-server client wiring (Phase 1.f.desktop.1).
//!
//! Three responsibilities:
//!
//! 1. **Server URL persistence** — the base URL of the
//!    `waveflow-server` deployment the active profile talks to. Stored
//!    in `app_setting['app.waveflow_server_url']` (app-wide; most
//!    users point one desktop install at one server, regardless of
//!    profile). Empty value = not configured, the rest of the app
//!    treats this as "local-only mode".
//!
//! 2. **JWT persistence** — Bearer JWT minted by Better Auth (via
//!    `waveflow-web`'s `auth.api.getToken`). Stored per-profile in
//!    `auth_credential` under the new `'waveflow_server'` provider,
//!    matching the existing Last.fm / Spotify pattern. The blob holds
//!    the raw JWT bytes today; a future hardening pass can move it to
//!    the OS keyring without changing this module's surface.
//!
//! 3. **HTTP client builder** — `WaveflowServerClient::request(method,
//!    path)` returns a [`reqwest::RequestBuilder`] pre-baked with the
//!    server's base URL + the stored Bearer header. Hot path for every
//!    future sync / CRUD call from the desktop.
//!
//! Out of scope (deferred to 1.f.desktop.1b / 1.f.desktop.4):
//!
//! - JWT refresh on 401 (needs the Better Auth refresh-token endpoint,
//!   which we'll surface as a server-fn on `waveflow-web` first).
//! - OS-keyring storage (the `keyring` crate ships fine on macOS /
//!   Windows but the Linux story relies on libsecret + a running
//!   secret service, which we want to validate against an AppImage
//!   build before committing to it).
//! - Local-loopback OAuth login flow — mirrors the existing
//!   `commands::spotify` pattern, but the matching `/desktop-login`
//!   route lives on `waveflow-web` and ships in its own PR.

use chrono::Utc;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// `app_setting` key for the base URL. App-wide on purpose — see the
/// module docstring.
pub const SERVER_URL_KEY: &str = "app.waveflow_server_url";

/// `app_setting` key for the `waveflow-web` URL — the Better Auth
/// frontend the OAuth-loopback handshake (Phase 1.f.desktop.1b) opens
/// in the system browser. May or may not be the same host as the
/// server URL; we keep them separate so a deployment that proxies the
/// two through different domains (e.g. `api.waveflow.app` vs
/// `waveflow.app`) is configurable without a derivation rule.
pub const WEB_URL_KEY: &str = "app.waveflow_web_url";

/// `auth_credential.provider` value reserved for the waveflow-server
/// JWT. Must match the CHECK constraint in the
/// `20260601000000_waveflow_server_auth_provider` migration.
pub const PROVIDER: &str = "waveflow_server";

/// Snapshot returned to the frontend so a Settings card can render
/// status without having to chain a `getUrl + isSignedIn` pair of
/// invokes. Sent back from `server_get_status` and every mutating
/// command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerStatus {
    /// Base URL the desktop talks to. `None` means the user hasn't
    /// configured one; the rest of the app should treat that as
    /// "local-only mode".
    pub url: Option<String>,
    /// `waveflow-web` URL used by the OAuth-loopback handshake. `None`
    /// when not yet configured; the OAuth flow refuses to start
    /// without it and points the user back at Settings.
    pub web_url: Option<String>,
    /// `true` when a JWT row exists for the active profile under the
    /// `'waveflow_server'` provider. Does NOT validate the token
    /// against the server — that's an HTTP round-trip the caller can
    /// do separately if they want a fresher signal.
    pub signed_in: bool,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Read the persisted server URL. Trim + filter-empty so an
/// `app_setting` row that was cleared by the user (set to `""`)
/// surfaces as `None` for the UI.
pub async fn read_url(app_db: &SqlitePool) -> AppResult<Option<String>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(SERVER_URL_KEY)
        .fetch_optional(app_db)
        .await?;
    Ok(raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

/// Upsert the server URL. Empty / whitespace-only input clears the
/// row entirely — same convention as
/// [`commands::integration::set_lastfm_api_key`].
///
/// Validates the URL is parseable + uses `http` / `https`; a typo'd
/// value would otherwise stick around and break every subsequent
/// `request()` build with a less helpful error.
pub async fn write_url(app_db: &SqlitePool, url: &str) -> AppResult<()> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        sqlx::query("DELETE FROM app_setting WHERE key = ?")
            .bind(SERVER_URL_KEY)
            .execute(app_db)
            .await?;
        return Ok(());
    }
    let parsed = url::Url::parse(trimmed)
        .map_err(|err| AppError::Other(format!("invalid server URL: {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Other("server URL must use http or https".into()));
    }
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(SERVER_URL_KEY)
    .bind(trimmed)
    .bind(now_ms())
    .execute(app_db)
    .await?;
    Ok(())
}

/// Read the JWT for the active profile. Returns `None` when no row
/// exists or the blob is empty — the UI treats both as "signed out".
pub async fn read_token(state: &AppState) -> AppResult<Option<String>> {
    let pool = match state.require_profile_pool().await {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let mut conn = pool.acquire().await?;
    read_token_conn(&mut conn).await
}

/// Same as [`read_token`] but takes a caller-owned connection so the
/// SELECT joins an open transaction. Used by
/// [`crate::sync::hooks::enqueue_op_in_tx`] to gate the outbox write
/// against the JWT presence inside the same atomic commit.
pub async fn read_token_conn(conn: &mut SqliteConnection) -> AppResult<Option<String>> {
    let row: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT token_encrypted FROM auth_credential WHERE provider = ?")
            .bind(PROVIDER)
            .fetch_optional(conn)
            .await?;
    let Some(bytes) = row else { return Ok(None) };
    let token = String::from_utf8(bytes)
        .map_err(|_| AppError::Other("waveflow_server JWT row is not valid UTF-8".into()))?;
    let trimmed = token.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Upsert the JWT. Trims + rejects empty input (use [`clear_token`]
/// for sign-out — leaving an empty blob would confuse the
/// `connected` heuristic).
pub async fn write_token(state: &AppState, token: &str) -> AppResult<()> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(AppError::Other("token must not be empty".into()));
    }
    // Cheap structural sanity check — three base64url segments
    // separated by dots. Doesn't validate the signature; that's the
    // server's job on the first authenticated request.
    if trimmed.split('.').count() != 3 {
        return Err(AppError::Other(
            "token does not look like a JWT (expected three dot-separated segments)".into(),
        ));
    }
    let pool = state.require_profile_pool().await?;
    let now = now_ms();
    sqlx::query(
        "INSERT INTO auth_credential
            (provider, username, token_encrypted, refresh_token_encrypted,
             expires_at, created_at, updated_at)
         VALUES (?, NULL, ?, NULL, NULL, ?, ?)
         ON CONFLICT(provider) DO UPDATE SET
            token_encrypted = excluded.token_encrypted,
            updated_at = excluded.updated_at",
    )
    .bind(PROVIDER)
    .bind(trimmed.as_bytes())
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await?;
    Ok(())
}

/// Drop the JWT row for the active profile. Idempotent — no error if
/// nothing was stored.
pub async fn clear_token(state: &AppState) -> AppResult<()> {
    let pool = match state.require_profile_pool().await {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };
    sqlx::query("DELETE FROM auth_credential WHERE provider = ?")
        .bind(PROVIDER)
        .execute(&pool)
        .await?;
    Ok(())
}

/// Snapshot helper — single round-trip the Settings UI uses to render
/// the "Compte serveur" card.
pub async fn status(state: &AppState) -> AppResult<ServerStatus> {
    let url = read_url(&state.app_db).await?;
    let web_url = read_web_url(&state.app_db).await?;
    let signed_in = read_token(state).await?.is_some();
    Ok(ServerStatus {
        url,
        web_url,
        signed_in,
    })
}

/// Read the persisted `waveflow-web` URL. Same trim-and-filter
/// semantics as [`read_url`].
pub async fn read_web_url(app_db: &SqlitePool) -> AppResult<Option<String>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(WEB_URL_KEY)
        .fetch_optional(app_db)
        .await?;
    Ok(raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

/// Persist the `waveflow-web` URL. Same validation gate as
/// [`write_url`] — `http(s)` only, empty input clears the row.
pub async fn write_web_url(app_db: &SqlitePool, url: &str) -> AppResult<()> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        sqlx::query("DELETE FROM app_setting WHERE key = ?")
            .bind(WEB_URL_KEY)
            .execute(app_db)
            .await?;
        return Ok(());
    }
    let parsed = url::Url::parse(trimmed)
        .map_err(|err| AppError::Other(format!("invalid web URL: {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Other("web URL must use http or https".into()));
    }
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(WEB_URL_KEY)
    .bind(trimmed)
    .bind(now_ms())
    .execute(app_db)
    .await?;
    Ok(())
}

/// HTTP client wired against the persisted URL + JWT. Build once per
/// caller — every method returns a fresh
/// [`reqwest::RequestBuilder`] so the caller can layer query params /
/// JSON body on top.
///
/// Errors map to [`AppError`] so handlers don't have to thread an
/// extra error type — same convention as [`crate::spotify`].
///
/// First consumer: [`crate::sync::drain`] (Phase 1.f.desktop.4a).
pub struct WaveflowServerClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl WaveflowServerClient {
    /// Build a client against the active profile's stored config.
    /// Returns `Err` when either the URL or the JWT is missing —
    /// callers that want a "no-op when offline" semantic should use
    /// [`try_build`] instead.
    ///
    /// Currently unused — [`try_build`] is the only consumer — but
    /// kept for parity with the contract docstring and for the WS
    /// subscriber landing in 1.f.desktop.4b, which needs the
    /// erroring variant for boot-time wiring.
    #[allow(dead_code)]
    pub async fn build(state: &AppState) -> AppResult<Self> {
        Self::try_build(state).await?.ok_or_else(|| {
            AppError::Other(
                "waveflow-server is not configured (set the URL and sign in first)".into(),
            )
        })
    }

    /// Build the client if both knobs are present, `Ok(None)`
    /// otherwise. The sync / WS surface in 1.f.desktop.2+ uses this to
    /// short-circuit cleanly when the user hasn't onboarded.
    pub async fn try_build(state: &AppState) -> AppResult<Option<Self>> {
        let Some(url) = read_url(&state.app_db).await? else {
            return Ok(None);
        };
        let Some(token) = read_token(state).await? else {
            return Ok(None);
        };
        // Strip the trailing slash so `format!("{base}/api/v1/…")`
        // never produces a double slash. A double slash would still
        // resolve on most servers but the OpenAPI spec rejects it,
        // and matching the convention upfront keeps the request
        // shape predictable in logs.
        let base_url = url.trim_end_matches('/').to_string();
        Ok(Some(Self {
            base_url,
            token,
            http: reqwest::Client::builder()
                // 30 s matches the server's tower-http timeout
                // default + the legacy convention elsewhere in the
                // desktop. A streaming download (1.f.desktop.4 +)
                // will need to override this on a per-request basis.
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|err| AppError::Other(format!("http client init: {err}")))?,
        }))
    }

    /// Build a request against `path` (e.g. `"/api/v1/profiles"`).
    /// Auto-prepends the persisted base URL and attaches the
    /// `Authorization: Bearer …` header.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = if path.starts_with('/') {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}/{}", self.base_url, path)
        };
        self.http.request(method, url).bearer_auth(&self.token)
    }

    /// Direct accessor — useful when a caller needs to hand the JWT
    /// off to another protocol (the WebSocket subscriber in
    /// 1.f.desktop.4 has to attach it via the upgrade headers, not
    /// the reqwest builder).
    #[allow(dead_code)]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Base URL accessor — same use case as [`token`].
    #[allow(dead_code)]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
