//! Authorization Code + PKCE against a native server.
//!
//! The desktop is a public client — it ships no secret a user could not
//! extract — so PKCE is what binds an authorization code to the running
//! instance that asked for it (RFC 7636). The consent screen is a route
//! of the server's own web client, which is why granting happens in the
//! system browser rather than in a window we control.
//!
//! ```text
//! bind 127.0.0.1:0  ──▶ port
//!         │
//! open <server>/authorize?client_id&redirect_uri&code_challenge&state&device_name
//!         │                       (browser session authenticates + consents)
//!         ▼
//! 127.0.0.1:<port>/wf/callback?code=…&state=…
//!         │
//! POST /api/v2/oauth/token {client_id, code, code_verifier, redirect_uri}
//!         ▼
//! access + refresh + device_id + account
//! ```
//!
//! ## Verified trap: a code is spent on first presentation
//!
//! Whatever the outcome. A wrong verifier burns it, and retrying the
//! same code is not a recovery path — the flow restarts from the
//! beginning. Nothing here retries an exchange.
//!
//! ## The port is ephemeral, and the URI is built exactly once
//!
//! RFC 8252 §7.3 tells a server not to pin the port of a loopback
//! redirect, and this one does not: it validates the *shape* — loopback
//! host, no fragment — and accepts any port. A fixed port would buy
//! nothing in exchange for two failure modes: anything else already
//! holding it makes sign-in impossible, and two sign-ins started at once
//! collide.
//!
//! But redemption is stricter than validation. The token endpoint
//! compares the grant's redirect URI with the one presented **as
//! strings** — measured against a live server, changing only the port
//! answers 401. So the value sent to `/authorize` and the value sent to
//! `/oauth/token` must be byte-identical. Binding first and building the
//! URI once, from the port we actually got, makes that true by
//! construction rather than by discipline.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tiny_http::Server;

use crate::{
    commands::loopback::html_response,
    error::{AppError, AppResult},
    offline,
    remote::{
        binding::{self, RemoteBinding, RemoteIdentity},
        client::AuthTokensResponse,
        probe::{self, ServerFlavour},
        tokens::{self, TokenPair},
    },
    state::AppState,
};

/// How we name ourselves to the server. Shown on the consent screen and
/// stored with the grant, so it has to be recognisable to a human.
const CLIENT_ID: &str = "waveflow-desktop";

/// Path the browser is redirected back to. Only the port varies.
const CALLBACK_PATH: &str = "/wf/callback";

/// Long enough to find a password manager, short enough that a
/// forgotten tab does not hold the listener open forever.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the browser came back with.
#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// The S256 challenge for a verifier: `base64url(sha256(verifier))`.
///
/// The same four lines exist in the Spotify integration. They are not
/// shared: that crate is a leaf about one third-party service, and
/// reaching into it from here for a hash helper would read as a
/// dependency that means something it does not.
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 256 bits as 64 hex characters — inside the 43..=128 the server
/// accepts, with no encoding subtleties to get wrong.
fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Run the whole handshake and bind the active profile to the server.
///
/// Returns the identity that was established. Any failure leaves the
/// previous binding untouched: nothing is written until the exchange has
/// succeeded.
pub async fn begin_login(state: &AppState, server_url: &str) -> AppResult<RemoteBinding> {
    if offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on; turn it off to sign in".into(),
        ));
    }

    let server_url = server_url.trim().trim_end_matches('/').to_string();
    let parsed = url::Url::parse(&server_url)
        .map_err(|err| AppError::Other(format!("invalid server URL: {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(AppError::Other("server URL must use http or https".into()));
    }

    // Refuse early rather than after sending the user to a consent
    // screen that does not exist.
    match probe::detect(&server_url).await? {
        ServerFlavour::Waveflow { .. } => {}
        ServerFlavour::Subsonic { server_type, .. } => {
            let name = server_type.unwrap_or_else(|| "this server".into());
            return Err(AppError::Other(format!(
                "{name} speaks the Subsonic protocol, which signs in with a username \
                 and password rather than through the browser"
            )));
        }
    }

    // Bind before anything else: the redirect URI has to carry the port
    // we actually got, and binding first also removes the race where a
    // fast browser reaches the callback before the listener is up.
    let server = Server::http("127.0.0.1:0").map_err(|err| {
        AppError::Other(format!("could not open a local port for sign-in: {err}"))
    })?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| AppError::Other("local sign-in listener has no IP address".into()))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

    let verifier = random_token();
    let challenge = pkce_challenge(&verifier);
    let csrf_state = random_token();

    let device_name = device_name();
    let authorize_url = format!(
        "{server_url}/authorize?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &csrf_state)
            .append_pair("device_name", &device_name)
            .finish()
    );

    let expected_state = csrf_state.clone();
    let callback =
        tauri::async_runtime::spawn_blocking(move || wait_for_code(server, &expected_state));

    tauri_plugin_opener::open_url(authorize_url, None::<&str>)
        .map_err(|err| AppError::Other(format!("could not open the browser: {err}")))?;

    let code = callback
        .await
        .map_err(|err| AppError::Other(format!("sign-in listener failed: {err}")))??;

    let tokens = exchange_code(&server_url, &code, &verifier, &redirect_uri).await?;

    persist_session(state, &server_url, tokens).await
}

/// The server rejects a device name longer than this, and it does so at
/// `/authorize` — which would fail the sign-in for a reason the user
/// could not possibly guess from a machine name.
const MAX_DEVICE_NAME: usize = 120;

/// How this install identifies itself in the account's device list.
/// The machine's hostname is what a user will recognise; falling back to
/// a generic label is better than failing the sign-in over it.
fn device_name() -> String {
    let name = match hostname() {
        Some(host) if !host.trim().is_empty() => format!("WaveFlow on {host}"),
        _ => "WaveFlow Desktop".to_string(),
    };
    truncate_device_name(name)
}

/// Truncate on a character boundary — the server counts bytes, and a
/// hostname may hold multi-byte characters.
fn truncate_device_name(mut name: String) -> String {
    if name.len() <= MAX_DEVICE_NAME {
        return name;
    }
    let mut end = MAX_DEVICE_NAME;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name.truncate(end);
    name
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|value| value.trim().to_string())
        })
        .filter(|value| !value.is_empty())
}

/// Block until the browser hits the callback, and hand back the code.
fn wait_for_code(server: Server, expected_state: &str) -> AppResult<String> {
    let request = server
        .recv_timeout(CALLBACK_TIMEOUT)
        .map_err(|err| AppError::Other(format!("sign-in callback failed: {err}")))?
        .ok_or_else(|| AppError::Other("sign-in timed out — try again".into()))?;

    let url = request.url().to_string();
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let parsed = serde_urlencoded::from_str::<CallbackQuery>(query)
        .map_err(|err| AppError::Other(format!("could not read the callback: {err}")))?;

    match (parsed.code, parsed.error, parsed.state.as_deref()) {
        // `error` absent is part of the success condition on purpose:
        // accepting a code that arrived alongside an error claim would
        // mask a protocol change instead of failing on it.
        (Some(code), None, state) if state == Some(expected_state) => {
            let _ = request.respond(html_response(
                "<!doctype html><title>WaveFlow</title>\
                 <p>Signed in. You can close this tab and return to WaveFlow.</p>",
            ));
            Ok(code)
        }
        (_, Some(error), _) => {
            let _ = request.respond(html_response(
                "<!doctype html><title>WaveFlow</title>\
                 <p>Sign-in was cancelled.</p>",
            ));
            // The consent screen's Deny button sends exactly this.
            if error == "access_denied" {
                Err(AppError::Other("sign-in was declined".into()))
            } else {
                Err(AppError::Other(format!("sign-in failed: {error}")))
            }
        }
        _ => {
            let _ = request.respond(html_response(
                "<!doctype html><title>WaveFlow</title>\
                 <p>Sign-in failed: the reply did not match this request. \
                 Start again from WaveFlow.</p>",
            ));
            Err(AppError::Other(
                "sign-in reply did not match this request (state mismatch)".into(),
            ))
        }
    }
}

/// Redeem the code. One attempt, by contract — see the module docs.
async fn exchange_code(
    server_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> AppResult<AuthTokensResponse> {
    let http = reqwest::Client::builder()
        .timeout(EXCHANGE_TIMEOUT)
        .build()
        .map_err(|err| AppError::Other(format!("http client init: {err}")))?;

    let response = http
        .post(format!("{server_url}/api/v2/oauth/token"))
        // No device name here: it belongs to the grant, which the
        // consent screen already created from the `/authorize` query.
        // Repeating it at redemption would be a field the token request
        // does not define.
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "code": code,
            "code_verifier": verifier,
            "redirect_uri": redirect_uri,
        }))
        .send()
        .await
        .map_err(|err| AppError::Other(format!("could not reach the server: {err}")))?;

    let status = response.status();
    if !status.is_success() {
        return Err(AppError::Other(format!(
            "the server refused the sign-in ({status})"
        )));
    }

    response
        .json::<AuthTokensResponse>()
        .await
        .map_err(|err| AppError::Other(format!("unexpected sign-in reply: {err}")))
}

/// Store the session, and decide what the previous one leaves behind.
///
/// Binding to a **different account** discards the projection and the
/// pending queue in the same transaction as the new binding. Both belong
/// to the account that produced them: a cursor is a position in that
/// account's journal, and a queued mutation names that account's
/// resources. Replaying either against a new account would apply one
/// user's edits to another's library.
///
/// Signing back into the **same** account keeps both, which is what
/// makes a re-authentication after an expired session cheap: the cursor
/// resumes and offline writes survive.
async fn persist_session(
    state: &AppState,
    server_url: &str,
    tokens: AuthTokensResponse,
) -> AppResult<RemoteBinding> {
    let profile_id = state.require_profile_id().await?;
    let pool = state.require_profile_pool_for(Some(profile_id)).await?;
    let mut tx = pool.begin().await?;

    let previous = binding::read(&mut tx).await?;
    let same_account = matches!(
        previous.as_ref().map(|b| &b.identity),
        Some(RemoteIdentity::Waveflow { account_id, .. }) if account_id == &tokens.user.id
    );

    if !same_account {
        binding::clear_account_state(&mut tx).await?;
    }

    let binding = RemoteBinding {
        server_url: server_url.to_string(),
        identity: RemoteIdentity::Waveflow {
            account_id: tokens.user.id.clone(),
            device_id: tokens.device_id.clone(),
            cursor: if same_account {
                previous
                    .as_ref()
                    .and_then(|b| b.identity.cursor())
                    .unwrap_or(0)
            } else {
                0
            },
        },
        active_library_id: if same_account {
            previous.as_ref().and_then(|b| b.active_library_id.clone())
        } else {
            None
        },
        bootstrapped_at: if same_account {
            previous.as_ref().and_then(|b| b.bootstrapped_at)
        } else {
            None
        },
    };
    binding::write(&mut tx, &binding).await?;

    tokens::write(
        &mut tx,
        &TokenPair {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at: tokens::deadline_from_expires_in(tokens.expires_in),
            username: Some(tokens.user.username),
        },
    )
    .await?;

    tx.commit().await?;
    Ok(binding)
}

/// Sign out without forgetting the server.
///
/// Drops the credentials and nothing else. The binding survives, so the
/// projection stays readable and a later sign-in to the same account
/// resumes from its cursor instead of re-downloading everything. Use
/// [`forget_server`] for the destructive version.
pub async fn sign_out(state: &AppState) -> AppResult<()> {
    let profile_id = state.require_profile_id().await?;
    let pool = state.require_profile_pool_for(Some(profile_id)).await?;
    let mut conn = pool.acquire().await?;
    tokens::clear(&mut conn).await
}

/// Forget the server entirely: credentials, binding, projection and any
/// write that never made it out. One transaction — a half-forgotten
/// server is worse than either state.
pub async fn forget_server(state: &AppState) -> AppResult<()> {
    let profile_id = state.require_profile_id().await?;
    let pool = state.require_profile_pool_for(Some(profile_id)).await?;
    let mut tx = pool.begin().await?;
    tokens::clear(&mut tx).await?;
    binding::clear_account_state(&mut tx).await?;
    binding::clear(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_what_the_server_will_accept() {
        // The server validates canonically: exactly 43 characters of
        // base64url decoding to a 32-byte digest. Anything else is
        // rejected before a grant is issued.
        let verifier = random_token();
        let challenge = pkce_challenge(&verifier);
        assert_eq!(challenge.len(), 43);
        assert_eq!(URL_SAFE_NO_PAD.decode(&challenge).unwrap().len(), 32);
        assert!(!challenge.contains('+') && !challenge.contains('/') && !challenge.contains('='));
    }

    #[test]
    fn the_verifier_sits_inside_the_length_the_server_allows() {
        let verifier = random_token();
        assert_eq!(verifier.len(), 64);
        assert!((43..=128).contains(&verifier.len()));
    }

    #[test]
    fn each_handshake_draws_fresh_secrets() {
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn a_known_verifier_hashes_to_its_known_challenge() {
        // Pinned against the RFC 7636 appendix B vector, so a future
        // change of encoder or digest cannot pass silently.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_device_name_never_comes_back_empty() {
        // It is sent on every sign-in and the server rejects an empty
        // one, so the fallback matters more than the hostname does.
        let name = device_name();
        assert!(!name.trim().is_empty());
        assert!(name.len() <= MAX_DEVICE_NAME);
    }

    #[test]
    fn a_long_hostname_is_trimmed_rather_than_failing_the_sign_in() {
        let name = truncate_device_name(format!("WaveFlow on {}", "h".repeat(400)));
        assert_eq!(name.len(), MAX_DEVICE_NAME);
    }

    #[test]
    fn truncation_lands_on_a_character_boundary() {
        // The server counts bytes; a hostname may not be ASCII. Cutting
        // mid-character would produce invalid UTF-8 and panic on
        // `truncate`.
        let name = truncate_device_name(format!("WaveFlow on {}", "é".repeat(200)));
        assert!(name.len() <= MAX_DEVICE_NAME);
        assert!(name.chars().next_back().is_some());
    }

    #[test]
    fn a_short_name_is_left_alone() {
        assert_eq!(
            truncate_device_name("WaveFlow on nas".into()),
            "WaveFlow on nas"
        );
    }
}
