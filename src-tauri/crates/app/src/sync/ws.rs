//! WebSocket subscriber + catch-up REST pull. Phase 1.f.desktop.4b.
//!
//! Closes the loop opened by the outbound drain task ([`crate::sync::drain`]):
//!
//! - **Catch-up pull**. Every connect attempt starts with a
//!   `GET /api/v1/sync/ops?since=N` loop where `N = profile_setting
//!   ['sync.last_seen_id']`. Each page (up to 1024 ops) is applied
//!   through [`crate::sync::apply::apply_remote_op_in_tx`] and the
//!   cursor advances inside the same tx. Loops until the server
//!   returns a short page; a `410 Gone` (compaction watermark above
//!   our cursor) surfaces in diagnostics and resets the cursor to 0
//!   so the next pass starts from the beginning.
//! - **Live WS subscribe**. After catch-up, opens
//!   `wss://<server>/api/v1/sync/ws?device_id=…` with the JWT in the
//!   `Authorization` header. Each `{"type":"op","op":{…}}` frame
//!   feeds the same apply path; the server's monotonic id advances
//!   the cursor and triggers an `{"ack": N}` frame upstream.
//! - **Reconnect with backoff**. Disconnects retry with exponential
//!   backoff (1s → 2s → 4s → … 60s cap). The gates (mode = Hybrid,
//!   JWT present, server URL configured) re-evaluate on every
//!   attempt so a user signing out / flipping to Local mode while
//!   the loop is asleep short-circuits the next iteration cleanly.
//!
//! ## Atomicity invariants
//!
//! Each remote op opens a fresh transaction:
//!
//! 1. `lamport::observe_remote_conn` bumps the local floor past the
//!    remote's `lamport_ts`.
//! 2. The apply dispatcher writes the entity row + sync_id_map row.
//! 3. `cursor::advance_conn` moves `sync.last_seen_id` past the op's
//!    server id.
//!
//! All three commit or roll back together. A crash mid-op (power
//! loss, panicked apply path) leaves the cursor unmoved, so the next
//! reconnect's catch-up replays the op.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde::Deserialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::Message;

use crate::{
    error::{AppError, AppResult},
    server_client::WaveflowServerClient,
    state::AppState,
    sync::{
        apply::{apply_remote_op_in_tx, AppliedOutcome, RemoteSyncOp},
        cursor, device, mode,
    },
};

/// Backoff floor — first retry waits 1 s.
const BACKOFF_MIN: Duration = Duration::from_secs(1);

/// Backoff ceiling — long enough that a permanently-down server
/// doesn't burn battery, short enough that recovery feels quick.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How long to wait between gate re-checks when the subscriber is
/// gated off (no JWT, Local mode, etc.). A user signing in pokes the
/// [`SubscribeHandle::wake`] so the loop doesn't sit on this floor
/// when something has actually changed.
const IDLE_GATE_INTERVAL: Duration = Duration::from_secs(30);

/// Wake signal the rest of the app uses to nudge the subscriber after
/// a sign-in / mode flip / server URL change. Same pattern as the
/// drain task's [`crate::sync::drain::DrainHandle`].
#[derive(Default)]
pub struct SubscribeHandle {
    notifier: Notify,
}

impl SubscribeHandle {
    /// Wake the subscriber on its next idle wait — typically called
    /// from the `server_account` Tauri commands after persisting a
    /// JWT or URL change.
    pub fn wake(&self) {
        self.notifier.notify_one();
    }
}

/// Outcome of a single end-to-end pass (catch-up + WS session +
/// disconnect). Surfaced for the diagnostic Tauri commands the next
/// PR layer can plug in.
#[derive(Debug, Clone, Copy)]
pub enum SubscribeOutcome {
    /// Gates blocked the pass — no HTTP, no WS, no work done.
    Skipped,
    /// At least one op (catch-up or live) was applied. Counts are
    /// surfaced for the future Settings diagnostic — kept on the
    /// variant so the existing call sites don't need a separate
    /// stats struct.
    #[allow(dead_code)]
    Ran {
        catchup_applied: usize,
        live_applied: usize,
    },
    /// The pass ran but no ops landed — typically the connect window
    /// where the server had nothing to push.
    Quiet,
}

// ─ Wire shapes ──────────────────────────────────────────────────────

/// `GET /api/v1/sync/ops?since=N` reply. Mirrors
/// `waveflow_server::api::sync::PullResponse`.
#[derive(Debug, Deserialize)]
struct PullResponse {
    ops: Vec<RemoteSyncOp>,
    last_id: i64,
}

/// 410 Gone body when `since < compacted_up_to`. The subscriber
/// resets the cursor to 0 so the next pass starts from the top.
#[derive(Debug, Deserialize)]
struct ResurrectedGone {
    #[serde(default)]
    #[allow(dead_code)]
    error: String,
    compacted_up_to: i64,
}

/// Top-level frame the server sends on the WS. We only consume
/// `{"type":"op","op":{…}}` today.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ServerFrame {
    Op { op: RemoteSyncOp },
}

/// ACK frame the client sends after applying an op. Mirrors
/// `waveflow_server::api::sync::InboundAck`.
#[derive(serde::Serialize)]
struct AckFrame {
    ack: i64,
}

// ─ Lifecycle ────────────────────────────────────────────────────────

/// Spawn the subscriber. Reads the wake handle off [`AppState::ws`]
/// (planted by `lib.rs::run` after `app.manage(state)`) so a sign-in
/// event nudges the loop without holding a reference to the task
/// itself.
pub fn spawn(app: AppHandle) {
    let handle = app.state::<AppState>().ws.clone();
    tokio::spawn(async move {
        let mut backoff = BACKOFF_MIN;
        loop {
            let state = app.state::<AppState>();
            let outcome = match run_session(&state).await {
                Ok(o) => o,
                Err(err) => {
                    tracing::warn!(error = %err, "sync ws session failed");
                    SubscribeOutcome::Quiet
                }
            };
            match outcome {
                SubscribeOutcome::Skipped => {
                    // Gates blocked the pass. Reset backoff so a
                    // subsequent successful sign-in connects right
                    // away rather than waiting the previous cap.
                    backoff = BACKOFF_MIN;
                    tokio::select! {
                        _ = tokio::time::sleep(IDLE_GATE_INTERVAL) => {}
                        _ = handle.notifier.notified() => {}
                    }
                }
                SubscribeOutcome::Ran { .. } | SubscribeOutcome::Quiet => {
                    // Session ran (and either disconnected cleanly
                    // or failed). Apply backoff before retrying.
                    tracing::debug!(?backoff, "sync ws reconnect");
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = handle.notifier.notified() => {}
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
    });
}

/// Run one connect-catchup-subscribe-disconnect cycle. Public for
/// tests + the future diagnostic command surface.
pub async fn run_session(state: &AppState) -> AppResult<SubscribeOutcome> {
    let Some(client) = WaveflowServerClient::try_build(state).await? else {
        return Ok(SubscribeOutcome::Skipped);
    };
    let pool = state.require_profile_pool().await?;
    if mode::read(&pool).await? != mode::SyncMode::Hybrid {
        return Ok(SubscribeOutcome::Skipped);
    }
    let device_id = device::ensure(&state.app_db).await?;

    let catchup_applied = catchup_pull(&client, &pool, &device_id).await?;
    let live_applied = open_ws_session(&client, &pool, &device_id).await?;

    let any = catchup_applied + live_applied;
    if any == 0 {
        Ok(SubscribeOutcome::Quiet)
    } else {
        Ok(SubscribeOutcome::Ran {
            catchup_applied,
            live_applied,
        })
    }
}

// ─ Catch-up ─────────────────────────────────────────────────────────

/// Loop on `GET /api/v1/sync/ops?since=N` until the server returns a
/// short page. Each op is applied + cursor advances + ACK sent. The
/// catch-up MUST complete (or hit a clean 410) before we open the WS
/// so a backlog never races a live push.
async fn catchup_pull(
    client: &WaveflowServerClient,
    pool: &SqlitePool,
    device_id: &str,
) -> AppResult<usize> {
    let mut applied = 0usize;
    loop {
        let since = cursor::read(pool).await?;
        let resp = client
            .request(reqwest::Method::GET, "/api/v1/sync/ops")
            .query(&[("since", since.to_string())])
            .send()
            .await
            .map_err(|err| AppError::Other(format!("sync pull request: {err}")))?;

        let status = resp.status();
        if status == StatusCode::GONE {
            // Resurrected device — server compacted past our cursor.
            // Reset to 0 and pull again from the top. The
            // canonical-id mapping is idempotent, so re-applying
            // already-seen ops is a no-op beyond the wire cost.
            let body: ResurrectedGone = resp
                .json()
                .await
                .map_err(|err| AppError::Other(format!("sync pull 410 body parse: {err}")))?;
            tracing::warn!(
                compacted_up_to = body.compacted_up_to,
                "sync pull: 410 Gone — resetting cursor and re-pulling"
            );
            // We can't go below 0; advance is monotonic. Use the
            // module-level reset so any future "cursor wiped"
            // side-effects (logging, events) live next to the cursor
            // logic instead of an inline DELETE here.
            cursor::reset(pool).await?;
            continue;
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Other(format!("sync pull HTTP {status}: {body}")));
        }
        let page: PullResponse = resp
            .json()
            .await
            .map_err(|err| AppError::Other(format!("sync pull body parse: {err}")))?;

        let was_empty = page.ops.is_empty();
        for op in &page.ops {
            let mut tx = pool.begin().await?;
            let outcome = apply_remote_op_in_tx(&mut tx, op, device_id).await?;
            cursor::advance_conn(&mut tx, op.id).await?;
            tx.commit().await?;
            // Same accounting as the WS branch — only count ops that
            // actually wrote to the local DB. Echoes/ignores still
            // advance the cursor without inflating `catchup_applied`.
            if matches!(outcome, AppliedOutcome::Applied) {
                applied += 1;
            }
        }
        // ACK the page end so the server's per-device cursor row
        // climbs even if no new ops arrive on the WS for a while.
        // The REST ACK + the eventual WS `{"ack": N}` both advance
        // the same buffer; doing both is defensive (server flushes
        // periodically + on WS disconnect; REST is the surest path
        // before the upgrade window).
        if page.last_id > since {
            let ack_payload = serde_json::json!({
                "device_id": device_id,
                "last_seen_id": page.last_id,
            });
            let _ = client
                .request(reqwest::Method::POST, "/api/v1/sync/ack")
                .json(&ack_payload)
                .send()
                .await;
        }
        if was_empty {
            break;
        }
    }
    Ok(applied)
}

// ─ WebSocket session ───────────────────────────────────────────────

/// Open the WebSocket, route `{"type":"op",...}` frames into the
/// apply path, and send `{"ack": N}` back per applied op. Returns
/// when the socket closes for any reason (server-initiated, network
/// flap, decode error). The outer [`spawn`] loop re-runs the whole
/// session — including a fresh catch-up — so a missed push during a
/// disconnect window is recovered via REST on reconnect.
async fn open_ws_session(
    client: &WaveflowServerClient,
    pool: &SqlitePool,
    device_id: &str,
) -> AppResult<usize> {
    let ws_url = http_to_ws_url(client.base_url(), device_id)?;
    // Defense-in-depth: the Bearer JWT rides in the WS upgrade's
    // `Authorization` header, so a `ws://` connection to a
    // non-loopback host puts the credential in cleartext on the
    // wire. We still allow it (parity with the persisted-URL gate
    // in `server_client::write_url`, which accepts http:// for
    // self-hosted LAN deployments + localhost dev), but log a
    // visible warning per connect so a misconfigured production
    // setup is loud rather than silent. Loopback hosts are skipped
    // — there's no realistic eavesdropping risk on a local socket.
    if ws_url.starts_with("ws://") && !host_is_loopback(client.base_url()) {
        tracing::warn!(
            server_url = %client.base_url(),
            "sync ws: opening cleartext ws:// connection — JWT rides in upgrade headers without TLS; \
             use https:// for production deployments",
        );
    }
    let request = build_ws_request(&ws_url, client.token())?;

    let (ws_stream, _resp) = match tokio_tungstenite::connect_async(request).await {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(error = %err, "sync ws connect failed");
            return Ok(0);
        }
    };
    let (mut sender, mut receiver) = ws_stream.split();
    let mut applied = 0usize;

    while let Some(frame) = receiver.next().await {
        let frame = match frame {
            Ok(f) => f,
            Err(err) => {
                tracing::debug!(error = %err, "sync ws recv error, closing");
                break;
            }
        };
        let text = match frame {
            Message::Text(t) => t,
            Message::Binary(_) => continue,
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
            Message::Close(_) => break,
        };
        let parsed: ServerFrame = match serde_json::from_str(&text) {
            Ok(p) => p,
            Err(err) => {
                tracing::debug!(error = %err, body = %text, "sync ws frame parse failed");
                continue;
            }
        };
        let op = match parsed {
            ServerFrame::Op { op } => op,
        };
        let op_id = op.id;

        let mut tx = pool.begin().await?;
        let outcome = apply_remote_op_in_tx(&mut tx, &op, device_id).await?;
        cursor::advance_conn(&mut tx, op_id).await?;
        tx.commit().await?;
        // Only count ops that actually wrote to the local DB. Echoes
        // (self-broadcasts) and ignores (unknown entity / mapping
        // miss / malformed payload) still advance the cursor — they
        // just shouldn't inflate `live_applied`.
        if matches!(outcome, AppliedOutcome::Applied) {
            applied += 1;
        }
        // ACK back so the server can advance its per-device cursor
        // and the compaction job knows we've consumed up to op_id.
        let ack = AckFrame { ack: op_id };
        let ack_text = serde_json::to_string(&ack)
            .map_err(|err| AppError::Other(format!("ack serialise: {err}")))?;
        if sender.send(Message::text(ack_text)).await.is_err() {
            break;
        }
    }
    Ok(applied)
}

/// Translate the persisted `http(s)://host[:port]` base URL into the
/// matching `ws(s)://host[:port]/api/v1/sync/ws?device_id=…`. Falls
/// back to the trailing-slash-stripped value `base_url()` already
/// returns + appends the path manually rather than letting `url`
/// re-encode + risk losing the `?device_id=` shape across reqwest
/// versions.
///
/// `http://` → `ws://` is allowed by design — the persisted-URL
/// gate in [`crate::server_client::write_url`] accepts plain HTTP
/// so self-hosted LAN deployments + localhost dev work without
/// fighting the framework. The non-loopback `ws://` branch is
/// reported per-session as a `tracing::warn!` from
/// [`open_ws_session`] so a misconfigured production setup surfaces
/// loudly in logs.
fn http_to_ws_url(base: &str, device_id: &str) -> AppResult<String> {
    let trimmed = base.trim_end_matches('/');
    let lowered = trimmed.to_ascii_lowercase();
    let body = if let Some(rest) = lowered.strip_prefix("https://") {
        // Re-use the original-case host from `trimmed` so a
        // case-sensitive proxy path stays intact.
        format!("wss://{}", &trimmed[8..8 + rest.len()])
    } else if let Some(rest) = lowered.strip_prefix("http://") {
        // nosemgrep: detect-insecure-websocket -- intentional ws://
        // for self-hosted LAN + localhost dev; non-loopback hosts
        // are flagged at connect via `host_is_loopback`.
        format!("ws://{}", &trimmed[7..7 + rest.len()])
    } else {
        return Err(AppError::Other(format!(
            "server URL must start with http:// or https:// (got '{base}')"
        )));
    };
    let encoded = serde_urlencoded::to_string([("device_id", device_id)])
        .map_err(|err| AppError::Other(format!("encode device_id for ws upgrade: {err}")))?;
    Ok(format!("{body}/api/v1/sync/ws?{encoded}"))
}

/// `true` when the URL's host resolves to a loopback address
/// (`localhost`, `127.0.0.0/8`, `::1`) — i.e. an `http://` →
/// `ws://` downgrade carries no realistic eavesdropping risk
/// because the bytes never leave the machine. Anything else
/// (LAN IPs, public hostnames) triggers the cleartext warning
/// in [`open_ws_session`].
///
/// Parse failures fall back to "not loopback" — better to warn
/// once on an unparseable URL than silently treat an odd value
/// as safe.
fn host_is_loopback(base: &str) -> bool {
    let Ok(parsed) = url::Url::parse(base) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

/// Build the WS upgrade request with the Bearer token attached. The
/// server's axum middleware extracts the `Authorization` header
/// BEFORE routing to the WS handler, so the JWT has to ride in the
/// upgrade request — there's no in-band hello frame.
fn build_ws_request(ws_url: &str, token: &str) -> AppResult<Request<()>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = ws_url
        .into_client_request()
        .map_err(|err| AppError::Other(format!("ws upgrade request build: {err}")))?;
    let header_value = format!("Bearer {token}");
    req.headers_mut().insert(
        "Authorization",
        header_value
            .parse()
            .map_err(|err| AppError::Other(format!("invalid bearer header: {err}")))?,
    );
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_to_ws_url_rewrites_scheme_and_appends_path() {
        let out = http_to_ws_url("https://api.example.com", "dev-abc").unwrap();
        assert_eq!(
            out,
            "wss://api.example.com/api/v1/sync/ws?device_id=dev-abc"
        );

        let out = http_to_ws_url("http://localhost:8787/", "dev-xyz").unwrap();
        assert_eq!(out, "ws://localhost:8787/api/v1/sync/ws?device_id=dev-xyz");
    }

    #[test]
    fn http_to_ws_url_url_encodes_device_id() {
        // A device id with characters that need percent-encoding —
        // unlikely in practice (UUID v4) but the encoder must still
        // hold for safety.
        let out = http_to_ws_url("https://x.test", "abc/?&=").unwrap();
        assert!(out.starts_with("wss://x.test/api/v1/sync/ws?device_id="));
        assert!(out.contains("%2F"));
        assert!(out.contains("%3F"));
    }

    #[test]
    fn http_to_ws_url_rejects_other_schemes() {
        let err = http_to_ws_url("ftp://x", "d").unwrap_err();
        assert!(format!("{err}").contains("http"));
    }

    #[test]
    fn host_is_loopback_recognises_localhost_and_127_0_0_x_and_ipv6_one() {
        assert!(host_is_loopback("http://localhost"));
        assert!(host_is_loopback("http://localhost:8787"));
        assert!(host_is_loopback("https://LOCALHOST/path"));
        assert!(host_is_loopback("http://127.0.0.1"));
        assert!(host_is_loopback("http://127.0.0.1:8787"));
        assert!(host_is_loopback("http://127.1.2.3")); // 127.0.0.0/8
        assert!(host_is_loopback("http://[::1]"));
        assert!(host_is_loopback("http://[::1]:8787"));
    }

    #[test]
    fn host_is_loopback_rejects_lan_and_public_hosts() {
        assert!(!host_is_loopback("http://192.168.1.10"));
        assert!(!host_is_loopback("http://10.0.0.5:8787"));
        assert!(!host_is_loopback("http://api.example.com"));
        assert!(!host_is_loopback("https://prod.waveflow.app"));
        // Parse failure → "not loopback" so the warning still fires.
        assert!(!host_is_loopback("not a url"));
    }

    #[test]
    fn server_frame_op_parses() {
        let frame = serde_json::json!({
            "type": "op",
            "op": {
                "id": 42,
                "lamport_ts": 11,
                "device_id": "dev-b",
                "entity": "playlist",
                "entity_id": "00000000-0000-4000-8000-000000000000",
                "field": "name",
                "op": "set",
                "payload": { "value": "Soirée" }
            }
        });
        let parsed: ServerFrame = serde_json::from_value(frame).unwrap();
        let ServerFrame::Op { op } = parsed;
        assert_eq!(op.id, 42);
        assert_eq!(op.field.as_deref(), Some("name"));
    }

    #[test]
    fn unknown_top_level_type_is_rejected_not_panicking() {
        let frame = serde_json::json!({ "type": "future_thing", "data": null });
        let parsed: Result<ServerFrame, _> = serde_json::from_value(frame);
        assert!(parsed.is_err());
    }
}
