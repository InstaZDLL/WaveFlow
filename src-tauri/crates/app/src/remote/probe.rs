//! Server flavour detection.
//!
//! Decides whether the URL the user typed is a WaveFlow server — and so
//! whether the journal capability exists at all — before asking them for
//! any credential.
//!
//! ## Why `ping`, and why it works unauthenticated
//!
//! Every server speaking the Subsonic protocol answers `/rest/ping`, and
//! WaveFlow stamps `type="waveflow"` into that answer. Measured against
//! a live server (2026-08-10), the discriminating fields are present
//! **even when the ping itself fails**:
//!
//! ```text
//! GET /rest/ping?f=json                       → 400
//!   {"subsonic-response":{"error":{"code":10,…},"status":"failed",
//!    "type":"waveflow","serverVersion":"2.0.0-beta.0","openSubsonic":true}}
//!
//! GET /rest/ping?u=nobody&p=nothing&…&f=json  → 401
//!   {"subsonic-response":{"error":{"code":40,…},"status":"failed",
//!    "type":"waveflow",…}}
//! ```
//!
//! Two consequences, both load-bearing:
//!
//! - detection needs **no credentials**, so the sign-in screen can offer
//!   the right flow — browser consent for WaveFlow, username and
//!   password for anything else — before the user types anything;
//! - the probe must parse the body **regardless of HTTP status**. A
//!   probe that bailed on a non-2xx response would learn nothing from
//!   either of the answers above, which are the only two an
//!   unauthenticated caller can obtain.
//!
//! ## What not to use
//!
//! Not `getOpenSubsonicExtensions`: it returns an *empty* container
//! today. A client probing capabilities that way would conclude the
//! server offers nothing, while it in fact offers the entire v2 API. The
//! discriminant is `type`, not the extension list.

use std::time::Duration;

use serde::Deserialize;
use waveflow_core::error::describe_http_error;

use crate::{
    error::{AppError, AppResult},
    offline,
};

/// Client name we identify as in the Subsonic query string. Required by
/// the protocol; servers surface it in their logs.
const CLIENT_NAME: &str = "WaveFlow";
/// Protocol version we claim. 1.16.1 is the level the modern extensions
/// are defined against.
const PROTOCOL_VERSION: &str = "1.16.1";

/// Short on purpose: this runs while the user waits on a settings
/// screen, and a wrong URL should say so quickly rather than hang.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What we found at the other end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerFlavour {
    /// A native WaveFlow server: the journal, per-device ACK and
    /// mutation idempotency are all available.
    Waveflow {
        /// The server's own version string, for display.
        server_version: Option<String>,
    },
    /// A server speaking the Subsonic protocol but not ours. Catalogue
    /// and playback only — no journal, and no safe blind replay of a
    /// mutation whose response was lost.
    Subsonic {
        /// Whatever the server calls itself, kept verbatim for display.
        server_type: Option<String>,
        server_version: Option<String>,
    },
}

impl ServerFlavour {
    /// Whether the optional synchronization surface exists here.
    ///
    /// Not currently called — the sync paths gate on the `Waveflow`
    /// binding variant directly — but kept as the named capability check
    /// the flavour exists to express.
    #[allow(dead_code)]
    pub fn supports_sync(&self) -> bool {
        matches!(self, ServerFlavour::Waveflow { .. })
    }
}

#[derive(Debug, Deserialize)]
struct PingEnvelope {
    #[serde(rename = "subsonic-response")]
    response: Option<PingResponse>,
}

#[derive(Debug, Deserialize)]
struct PingResponse {
    #[serde(rename = "type")]
    server_type: Option<String>,
    #[serde(rename = "serverVersion")]
    server_version: Option<String>,
}

/// Identify the server behind `base_url`.
///
/// Errors only when we could not reach anything that looks like a music
/// server — a wrong URL, an unreachable host, or a reply that is not a
/// Subsonic envelope. A *failed* ping from a real server is a success
/// here: it still identifies itself.
pub async fn detect(base_url: &str) -> AppResult<ServerFlavour> {
    if offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on; turn it off to reach a server".into(),
        ));
    }

    let url = format!(
        "{}/rest/ping?{}",
        base_url.trim_end_matches('/'),
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("v", PROTOCOL_VERSION)
            .append_pair("c", CLIENT_NAME)
            .append_pair("f", "json")
            .finish(),
    );

    let http = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|err| AppError::Other(format!("http client init: {err}")))?;

    let response = http.get(url).send().await.map_err(|err| {
        AppError::Other(format!(
            "could not reach the server: {}",
            describe_http_error(&err)
        ))
    })?;

    // Deliberately not checking the status — see the module docs. An
    // unauthenticated ping answers 400 or 401 and still identifies the
    // server.
    let body = response.text().await.map_err(|err| {
        AppError::Other(format!(
            "could not read the server's reply: {}",
            describe_http_error(&err)
        ))
    })?;

    classify_ping_body(&body)
}

/// Parse a ping body into a flavour. Split out from the request so the
/// interesting half is testable without a server.
pub fn classify_ping_body(body: &str) -> AppResult<ServerFlavour> {
    let envelope: PingEnvelope = serde_json::from_str(body)
        .map_err(|_| AppError::Other("this URL did not answer like a music server".to_string()))?;

    let Some(response) = envelope.response else {
        return Err(AppError::Other(
            "this URL did not answer like a music server".to_string(),
        ));
    };

    match response.server_type.as_deref() {
        Some(server_type) if server_type.eq_ignore_ascii_case("waveflow") => {
            Ok(ServerFlavour::Waveflow {
                server_version: response.server_version,
            })
        }
        // Anything else that answered in the protocol is a third-party
        // server. It is a perfectly valid target — it simply has no
        // journal, so `SyncProvider` stays absent.
        other => Ok(ServerFlavour::Subsonic {
            server_type: other.map(str::to_owned),
            server_version: response.server_version,
        }),
    }
}

#[cfg(test)]
mod tests;
