//! Tauri commands for binding a profile to a remote server (RFC-005).
//!
//! Deliberately small: identifying a server, signing in, and the two
//! ways of undoing that. Everything the projection does sits behind the
//! binding these commands establish.
//!
//! ## Two ways out, because they are not the same intent
//!
//! [`remote_sign_out`] drops the credentials only. The binding and the
//! projection survive, so the cached remote library stays readable and
//! signing back into the same account resumes from its cursor.
//!
//! [`remote_forget_server`] drops everything, pending writes included.
//! That is the destructive one, and the UI must present it as such.

use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    remote::{
        auth,
        binding::{self, RemoteIdentity},
        probe::{self, ServerFlavour},
        tokens,
    },
    state::AppState,
};

/// What the Settings card renders. One round-trip rather than a chain
/// of `getUrl` + `isSignedIn` calls.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteStatus {
    /// `None` means this profile is local-only, which is the normal
    /// state, not a fault.
    pub server_url: Option<String>,
    /// `"waveflow"` or `"subsonic"`, absent when unbound.
    pub flavour: Option<String>,
    /// The signed-in account's name, when we know it.
    pub username: Option<String>,
    /// Whether usable credentials exist. Does not prove the server still
    /// accepts them — that is one HTTP round-trip away and belongs to
    /// whoever needs the fresher signal.
    pub signed_in: bool,
    /// Whether a first snapshot has ever been applied. Distinguishes an
    /// empty account from a bootstrap that never ran.
    pub bootstrapped: bool,
}

/// What a probe found, before any credential is asked for.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteProbeResult {
    pub flavour: String,
    /// Whatever the server calls itself, kept verbatim for display.
    pub server_type: Option<String>,
    pub server_version: Option<String>,
    /// Whether the journal, per-device acknowledgement and mutation
    /// idempotency exist here.
    pub supports_sync: bool,
}

#[tauri::command]
pub async fn remote_get_status(state: tauri::State<'_, AppState>) -> AppResult<RemoteStatus> {
    status(&state).await
}

/// Identify the server behind a URL without signing in.
///
/// Useful before the user commits to anything: a native server signs in
/// through the browser, a third-party one with a username and password,
/// and telling them apart afterwards would mean showing the wrong form
/// first.
#[tauri::command]
pub async fn remote_detect_server(url: String) -> AppResult<RemoteProbeResult> {
    let flavour = probe::detect(url.trim()).await?;
    Ok(match flavour {
        ServerFlavour::Waveflow { server_version } => RemoteProbeResult {
            flavour: "waveflow".into(),
            server_type: Some("waveflow".into()),
            server_version,
            supports_sync: true,
        },
        ServerFlavour::Subsonic {
            server_type,
            server_version,
        } => RemoteProbeResult {
            flavour: "subsonic".into(),
            server_type,
            server_version,
            supports_sync: false,
        },
    })
}

/// Run the browser handshake and bind the active profile.
///
/// Blocks for as long as the user takes to consent, up to three minutes.
/// The frontend should show this as a pending state rather than an
/// unresponsive one.
#[tauri::command]
pub async fn remote_begin_login(
    state: tauri::State<'_, AppState>,
    url: String,
) -> AppResult<RemoteStatus> {
    auth::begin_login(&state, &url).await?;
    status(&state).await
}

/// Drop the credentials, keep everything else.
#[tauri::command]
pub async fn remote_sign_out(state: tauri::State<'_, AppState>) -> AppResult<RemoteStatus> {
    auth::sign_out(&state).await?;
    status(&state).await
}

/// Drop the credentials, the binding, the projection and every write
/// that never reached the server.
#[tauri::command]
pub async fn remote_forget_server(state: tauri::State<'_, AppState>) -> AppResult<RemoteStatus> {
    auth::forget_server(&state).await?;
    status(&state).await
}

async fn status(state: &AppState) -> AppResult<RemoteStatus> {
    // No active profile is a legitimate state here — the onboarding
    // screens can query this before one exists — so it answers "unbound"
    // rather than failing. Anything else is a real fault and propagates.
    let pool = match state.require_profile_pool().await {
        Ok(pool) => pool,
        Err(AppError::NoActiveProfile) => {
            return Ok(RemoteStatus {
                server_url: None,
                flavour: None,
                username: None,
                signed_in: false,
                bootstrapped: false,
            })
        }
        Err(err) => return Err(err),
    };
    let mut conn = pool.acquire().await?;

    let binding = binding::read(&mut conn).await?;
    let credentials = tokens::read(&mut conn).await?;

    Ok(RemoteStatus {
        server_url: binding.as_ref().map(|b| b.server_url.clone()),
        flavour: binding.as_ref().map(|b| b.identity.flavour().to_string()),
        username: credentials
            .as_ref()
            .and_then(|c| c.username.clone())
            .or_else(|| match binding.as_ref().map(|b| &b.identity) {
                Some(RemoteIdentity::Subsonic { username }) => Some(username.clone()),
                _ => None,
            }),
        signed_in: credentials.is_some(),
        bootstrapped: binding
            .as_ref()
            .is_some_and(|b| b.bootstrapped_at.is_some()),
    })
}
