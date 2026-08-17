//! The wake-up socket.
//!
//! `GET /api/v2/sync/socket?after=<cursor>` upgrades to a WebSocket that
//! sends `{"cursor": 43}` when the account's journal grows.
//!
//! ## It carries notice, never state
//!
//! The frames are a hint that a newer cursor may exist. Everything the
//! client actually applies comes from `/sync/changes`, and the durable
//! cursor — not socket delivery — is the guarantee. That is why a
//! reconnect, a timeout or a frame we cannot parse all lead to the same
//! place as a notice: ask the journal. A dropped frame therefore costs
//! latency, never correctness.
//!
//! Which also means this whole module is an optimisation. With it
//! removed, [`super::sync::sync_now`] on a timer still converges; what
//! the socket buys is that an edit made on a phone shows up in seconds
//! instead of at the next poll.
//!
//! ## Reconnection
//!
//! Exponential backoff, reset after a session that actually delivered
//! something. An unbound profile, a signed-out one and offline mode are
//! not failures — the loop idles at the base interval instead of
//! escalating, because escalating would delay the reconnect that follows
//! the user signing back in.

use std::time::Duration;

use futures::StreamExt;
use tauri::{AppHandle, Emitter, Manager};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    error::{AppError, AppResult},
    offline,
    remote::{binding, client::RemoteClient, sync},
    state::AppState,
};

/// Event the frontend listens on to refresh what it shows.
const SYNCED_EVENT: &str = "remote:synced";

/// Wait between reconnection attempts, and the idle interval when there
/// is nothing to connect to.
const BASE_BACKOFF: Duration = Duration::from_secs(5);
/// Ceiling for the backoff. Five minutes keeps a long outage from
/// hammering a server that is down, while still recovering promptly once
/// it returns.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Start the subscriber. Returns immediately; the loop runs for the
/// lifetime of the app.
pub fn spawn(handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut backoff = BASE_BACKOFF;
        loop {
            let state = handle.state::<AppState>();
            match run_session(&handle, state.inner()).await {
                Ok(delivered) => {
                    // A session that did some work proves the path is
                    // healthy; one that returned straight away (no
                    // binding, offline) proves nothing, so it must not
                    // reset an escalating backoff either.
                    backoff = if delivered { BASE_BACKOFF } else { backoff };
                }
                Err(error) => {
                    tracing::debug!(%error, "sync socket session ended");
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
            tokio::time::sleep(backoff).await;
        }
    });
}

/// One connection, from upgrade to close.
///
/// `Ok(true)` when the session actually delivered a notice, which is
/// what tells the caller the path is healthy.
async fn run_session(handle: &AppHandle, state: &AppState) -> AppResult<bool> {
    if offline::is_offline() {
        return Ok(false);
    }
    let Some(client) = RemoteClient::try_build(state).await? else {
        // Unbound, signed out, or a server with no journal.
        return Ok(false);
    };

    let cursor = {
        let profile_id = state.require_profile_id().await?;
        let pool = state.require_profile_pool_for(Some(profile_id)).await?;
        let mut conn = pool.acquire().await?;
        binding::read(&mut conn)
            .await?
            .and_then(|b| b.identity.cursor())
            .unwrap_or(0)
    };

    let url = ws_url(client.base_url(), cursor)?;
    if is_cleartext_to_a_remote_host(&url) {
        // Self-hosted LAN and localhost are legitimate, so this is a
        // warning rather than a refusal — but the access token rides in
        // the upgrade headers, so a public host over plain ws:// is
        // worth saying out loud once per session.
        tracing::warn!(
            "sync socket is connecting in cleartext to a non-loopback host; \
             the access token rides in the upgrade headers unencrypted"
        );
    }

    let request = build_request(&url, client.access_token())?;
    let (stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|err| AppError::Other(format!("sync socket upgrade failed: {err}")))?;
    let (_writer, mut reader) = stream.split();

    // Reconnecting is itself a reason to ask: anything that happened
    // while we were away arrived through no frame at all.
    catch_up_and_notify(handle, state).await;

    let mut delivered = false;
    while let Some(message) = reader.next().await {
        match message {
            Ok(Message::Text(text)) => {
                // The cursor in the frame is deliberately not compared
                // against ours. It is a hint, and `/sync/changes` is
                // cheap when there is nothing new: one request answering
                // an empty page. Trusting the hint enough to skip the
                // fetch would make a stale or reordered frame lose an
                // event.
                let _ = notice_cursor(&text);
                delivered = true;
                catch_up_and_notify(handle, state).await;
            }
            Ok(Message::Close(_)) => break,
            // Ping and Pong are handled by the library; anything else is
            // not part of this protocol and is ignored rather than
            // treated as an error.
            Ok(_) => {}
            Err(error) => {
                return Err(AppError::Other(format!("sync socket read failed: {error}")));
            }
        }
    }

    Ok(delivered)
}

async fn catch_up_and_notify(handle: &AppHandle, state: &AppState) {
    match sync::sync_now(state).await {
        Ok(report) => {
            // Only wake the UI when something actually changed. Emitting
            // on every empty page would re-render the library on a timer
            // for no reason.
            if report.applied > 0 || report.resnapshotted {
                let _ = handle.emit(SYNCED_EVENT, report.cursor);
            }
        }
        Err(error) => tracing::debug!(%error, "catch-up after a socket notice failed"),
    }
}

/// Read the cursor out of a notice frame.
///
/// A frame we cannot parse is not fatal — the caller fetches regardless
/// — so this returns `None` rather than an error.
fn notice_cursor(text: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get("cursor")?
        .as_i64()
}

/// `http(s)://host` → `ws(s)://host/api/v2/sync/socket?after=N`.
///
/// Built by string surgery on the scheme rather than through `Url`,
/// which is what the v1 subscriber settled on: re-parsing risks
/// re-encoding a proxy path, and the host's original case must survive
/// for a case-sensitive proxy.
fn ws_url(base: &str, after: i64) -> AppResult<String> {
    let trimmed = base.trim_end_matches('/');
    let lowered = trimmed.to_ascii_lowercase();
    let body = if let Some(rest) = lowered.strip_prefix("https://") {
        format!("wss://{}", &trimmed[8..8 + rest.len()])
    } else if let Some(rest) = lowered.strip_prefix("http://") {
        // nosemgrep: detect-insecure-websocket -- plain ws:// is
        // intentional for self-hosted LAN and localhost; a non-loopback
        // host is warned about at connect time.
        format!("ws://{}", &trimmed[7..7 + rest.len()])
    } else {
        return Err(AppError::Other(format!(
            "server URL must start with http:// or https:// (got '{base}')"
        )));
    };
    Ok(format!("{body}/api/v2/sync/socket?after={after}"))
}

/// Whether this is plain `ws://` to something other than this machine.
///
/// An unparseable URL counts as remote: warning once about an odd value
/// beats silently treating it as safe.
fn is_cleartext_to_a_remote_host(ws_url: &str) -> bool {
    if !ws_url.starts_with("ws://") {
        return false;
    }
    let Ok(parsed) = url::Url::parse(ws_url) else {
        return true;
    };
    match parsed.host() {
        Some(url::Host::Domain(host)) => !host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => !addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => !addr.is_loopback(),
        None => true,
    }
}

/// The upgrade request, with the access token attached.
///
/// The server authenticates the upgrade with the same Bearer header as
/// REST, and its middleware reads it before routing — so the token has
/// to ride on the upgrade itself. There is no in-band hello frame.
fn build_request(
    ws_url: &str,
    access_token: &str,
) -> AppResult<tokio_tungstenite::tungstenite::handshake::client::Request> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = ws_url
        .into_client_request()
        .map_err(|err| AppError::Other(format!("sync socket request build: {err}")))?;
    let value = format!("Bearer {access_token}");
    request.headers_mut().insert(
        "Authorization",
        value
            .parse()
            .map_err(|err| AppError::Other(format!("invalid bearer header: {err}")))?,
    );
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_upgrades_to_wss_and_http_to_ws() {
        assert_eq!(
            ws_url("https://music.example", 12).unwrap(),
            "wss://music.example/api/v2/sync/socket?after=12"
        );
        assert_eq!(
            ws_url("http://localhost:4533", 0).unwrap(),
            "ws://localhost:4533/api/v2/sync/socket?after=0"
        );
    }

    #[test]
    fn a_trailing_slash_does_not_double_up_the_path() {
        assert_eq!(
            ws_url("https://music.example/", 1).unwrap(),
            "wss://music.example/api/v2/sync/socket?after=1"
        );
    }

    #[test]
    fn the_hosts_case_survives_for_a_case_sensitive_proxy() {
        assert!(ws_url("https://Music.Example/Path", 1)
            .unwrap()
            .starts_with("wss://Music.Example/Path"));
    }

    #[test]
    fn a_scheme_we_cannot_upgrade_is_an_error() {
        assert!(ws_url("ftp://music.example", 0).is_err());
        assert!(ws_url("music.example", 0).is_err());
    }

    #[test]
    fn cleartext_is_only_flagged_when_it_leaves_the_machine() {
        assert!(!is_cleartext_to_a_remote_host(
            "ws://localhost:4533/api/v2/sync/socket?after=0"
        ));
        assert!(!is_cleartext_to_a_remote_host(
            "ws://127.0.0.1:4533/api/v2/sync/socket?after=0"
        ));
        assert!(!is_cleartext_to_a_remote_host("ws://[::1]:4533/x"));
        assert!(is_cleartext_to_a_remote_host("ws://nas.lan/x"));
        assert!(is_cleartext_to_a_remote_host("ws://198.51.100.7/x"));
        // Encrypted is never flagged, wherever it points.
        assert!(!is_cleartext_to_a_remote_host("wss://music.example/x"));
    }

    #[test]
    fn an_unparseable_url_is_treated_as_remote() {
        assert!(is_cleartext_to_a_remote_host("ws://"));
    }

    #[test]
    fn a_notice_frame_yields_its_cursor() {
        assert_eq!(notice_cursor(r#"{"cursor": 43}"#), Some(43));
    }

    #[test]
    fn a_frame_we_cannot_read_is_not_fatal() {
        // The caller fetches regardless, so an unreadable frame costs
        // nothing but the parse attempt.
        assert_eq!(notice_cursor("not json"), None);
        assert_eq!(notice_cursor(r#"{"other": 1}"#), None);
        assert_eq!(notice_cursor(r#"{"cursor": "43"}"#), None);
    }
}
