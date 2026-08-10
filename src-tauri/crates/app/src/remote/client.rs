//! HTTP surface for the native `/api/v2` server.
//!
//! Four jobs, none of which the caller should have to think about:
//!
//! 1. **Bearer on every request**, base URL prepended once.
//! 2. **Idempotency headers on mutations** — `X-WaveFlow-Operation-Id`
//!    and `X-WaveFlow-Device-Id`, which is what lets the outbound queue
//!    re-issue a call after a lost response without duplicating the
//!    effect. Only the `/api/v2` routes read them, which is the direct
//!    reason RFC-005 §Decision 2 forbids routing our own traffic through
//!    the compatibility façade.
//! 3. **Refresh once on a 401**, single-flighted process-wide.
//! 4. **Classify failures** as permanent or transient, so the queue
//!    knows whether retrying can ever help.
//!
//! ## Why refresh has to be single-flighted
//!
//! Refresh tokens rotate: spending one mints a new pair and kills the
//! old. Two tasks that 401 at the same moment and both refresh do not
//! merely duplicate work — the second presents a token the first has
//! already spent, gets a 401 back, and concludes the profile is signed
//! out while the server holds a perfectly valid session.
//!
//! The gate is process-wide rather than per-client because two clients
//! built from the same profile share the credential row. After taking
//! the gate, [`refresh`] **re-reads** the stored pair: if it changed
//! while we queued, somebody else already refreshed and we adopt their
//! result instead of spending our now-stale token. Without that second
//! read the gate would only serialize the damage, not prevent it.

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::{
    error::{AppError, AppResult},
    offline,
    remote::{
        binding::{self, RemoteBinding, RemoteIdentity},
        tokens::{self, TokenPair},
    },
    state::AppState,
};

/// Header the server reads to recognize a replayed mutation.
const OPERATION_ID_HEADER: &str = "X-WaveFlow-Operation-Id";
/// Header identifying the device a mutation originated from. The server
/// rejects a device owned by another account.
const DEVICE_ID_HEADER: &str = "X-WaveFlow-Device-Id";

/// Refresh this long before the access token actually lapses. A token
/// with two seconds left passes a naive validity test and then 401s
/// mid-flight; one spare round-trip is cheaper than that retry.
const REFRESH_MARGIN_MS: i64 = 30_000;

/// Matches the server's own request timeout, so a client-side abort
/// does not race a response that was about to arrive.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Process-wide refresh gate. See the module docs.
fn refresh_gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

/// Why a request failed, reduced to the only distinction the caller
/// actually acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Retrying cannot change the outcome. The caller stops and
    /// surfaces the problem.
    ///
    /// This covers the idempotency conflict: reusing an operation
    /// identifier with a different fingerprint answers `422`, the same
    /// status as a malformed payload. The two are indistinguishable on
    /// the wire and, usefully, call for the same handling.
    Permanent,
    /// The condition may clear on its own — congestion, a restart, a
    /// flaky link. Worth retrying with backoff.
    Transient,
    /// Credentials were refused. Handled internally by one refresh; if
    /// it reaches the caller, the profile is genuinely signed out.
    Unauthorized,
}

/// Classify an HTTP status.
///
/// `408` and `429` sit with the 5xx range rather than with the other
/// 4xx: they describe a moment, not a malformed request.
pub fn classify_status(status: u16) -> Option<FailureKind> {
    match status {
        200..=299 => None,
        401 => Some(FailureKind::Unauthorized),
        408 | 429 => Some(FailureKind::Transient),
        500..=599 => Some(FailureKind::Transient),
        _ => Some(FailureKind::Permanent),
    }
}

/// A failed call, with enough context to be actionable in a log or a
/// diagnostics panel.
#[derive(Debug, Clone)]
pub struct RemoteFailure {
    pub kind: FailureKind,
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for RemoteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "{status}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl RemoteFailure {
    fn transport(error: reqwest::Error) -> Self {
        Self {
            // A transport error is a statement about the link, never
            // about the request's validity — always worth retrying.
            kind: FailureKind::Transient,
            status: None,
            message: error.to_string(),
        }
    }
}

/// The server's error body. Both fields are always present.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

/// Shape returned by `/auth/login`, `/auth/refresh` and `/oauth/token`.
#[derive(Debug, Deserialize)]
pub struct AuthTokensResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub device_id: String,
    pub user: AuthUserResponse,
}

#[derive(Debug, Deserialize)]
pub struct AuthUserResponse {
    pub id: String,
    pub username: String,
}

/// A client bound to one profile's server and credentials.
///
/// Built per operation rather than held long-term: the credential it
/// carries is a snapshot, and a client kept across a refresh would go on
/// presenting a token that has been rotated away.
pub struct RemoteClient {
    base_url: String,
    device_id: Option<String>,
    access_token: String,
    http: reqwest::Client,
}

impl RemoteClient {
    /// Build against the active profile's binding and tokens, or
    /// `Ok(None)` when the profile is not bound to a native server.
    ///
    /// Returns `None` — not an error — for an unbound profile, a
    /// signed-out one, or a Subsonic binding: local-only is the normal
    /// case, and a server without a journal has no business here.
    pub async fn try_build(state: &AppState) -> AppResult<Option<Self>> {
        // Capture the profile up front and pin every later acquisition to
        // it. A switch landing mid-way must fail this call rather than
        // read one profile's binding and write another profile's tokens.
        let profile_id = state.require_profile_id().await?;

        let (binding, pair) = {
            let pool = state.require_profile_pool_for(Some(profile_id)).await?;
            let mut conn = pool.acquire().await?;

            let Some(binding) = binding::read(&mut conn).await? else {
                return Ok(None);
            };
            if !matches!(binding.identity, RemoteIdentity::Waveflow { .. }) {
                return Ok(None);
            }
            let Some(pair) = tokens::read(&mut conn).await? else {
                return Ok(None);
            };
            (binding, pair)
            // The lease ends here, before any network call: holding it
            // across a 30-second request would stall a profile switch for
            // the whole duration.
        };

        // Pre-emptive refresh: cheaper than discovering the lapse
        // mid-request and unwinding.
        let pair = if pair.needs_refresh(chrono::Utc::now().timestamp_millis(), REFRESH_MARGIN_MS) {
            refresh(state, profile_id, &binding, &pair).await?
        } else {
            pair
        };

        Ok(Some(Self::from_parts(&binding, &pair)?))
    }

    fn from_parts(binding: &RemoteBinding, pair: &TokenPair) -> AppResult<Self> {
        Ok(Self {
            base_url: binding.server_url.trim_end_matches('/').to_string(),
            device_id: binding.identity.device_id().map(str::to_owned),
            access_token: pair.access_token.clone(),
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .map_err(|err| AppError::Other(format!("http client init: {err}")))?,
        })
    }

    /// A read request. No idempotency headers — a GET has nothing to
    /// replay.
    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(self.url(path)).bearer_auth(&self.access_token)
    }

    /// A mutation, stamped so the server can recognize a replay.
    ///
    /// `operation_id` must be the identifier stored alongside the queued
    /// entry and must never change for a given logical mutation: the
    /// server fingerprints the action, target and normalized payload,
    /// and answers a same-id-different-fingerprint call with a conflict
    /// rather than a replay.
    pub fn mutate(
        &self,
        method: reqwest::Method,
        path: &str,
        operation_id: &str,
    ) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .request(method, self.url(path))
            .bearer_auth(&self.access_token)
            .header(OPERATION_ID_HEADER, operation_id);
        if let Some(device_id) = &self.device_id {
            builder = builder.header(DEVICE_ID_HEADER, device_id);
        }
        builder
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with('/') {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}/{}", self.base_url, path)
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Send a request and decode its JSON body.
    ///
    /// Refuses to leave the machine while the process is in offline
    /// mode, and reports that as transient — offline is a state the user
    /// toggles back, not a reason to give up on a queued write.
    pub async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<T, RemoteFailure> {
        let response = self.send(builder).await?;
        response.json::<T>().await.map_err(|err| RemoteFailure {
            // A body we cannot parse is not a link problem: the server
            // answered, and its answer is not what the contract
            // promises. Retrying re-fetches the same bytes.
            kind: FailureKind::Permanent,
            status: None,
            message: format!("malformed response body: {err}"),
        })
    }

    /// Send a request, discarding a successful body. For the `204`
    /// mutations.
    pub async fn send_ok(&self, builder: reqwest::RequestBuilder) -> Result<(), RemoteFailure> {
        self.send(builder).await.map(|_| ())
    }

    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, RemoteFailure> {
        if offline::is_offline() {
            return Err(RemoteFailure {
                kind: FailureKind::Transient,
                status: None,
                message: "offline mode is on".into(),
            });
        }

        let response = builder.send().await.map_err(RemoteFailure::transport)?;
        let status = response.status().as_u16();
        match classify_status(status) {
            None => Ok(response),
            Some(kind) => {
                // Read the structured body while we still can — it names
                // the failure far better than the status alone.
                let message = match response.json::<ErrorBody>().await {
                    Ok(body) => format!("{} ({})", body.message, body.code),
                    Err(_) => "no error body".to_string(),
                };
                Err(RemoteFailure {
                    kind,
                    status: Some(status),
                    message,
                })
            }
        }
    }
}

/// Exchange the stored refresh token for a fresh pair and persist it.
///
/// Single-flighted process-wide, with a re-read after taking the gate so
/// a task that queued behind another adopts its result rather than
/// spending a token that has already been rotated away.
pub async fn refresh(
    state: &AppState,
    profile_id: i64,
    binding: &RemoteBinding,
    known: &TokenPair,
) -> AppResult<TokenPair> {
    let _guard = refresh_gate().lock().await;

    {
        let pool = state.require_profile_pool_for(Some(profile_id)).await?;
        let mut conn = pool.acquire().await?;

        // Somebody may have refreshed while we waited for the gate.
        if let Some(current) = tokens::read(&mut conn).await? {
            if current.refresh_token != known.refresh_token {
                return Ok(current);
            }
        }
        // Lease released before the request — see `try_build`.
    }

    let http = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| AppError::Other(format!("http client init: {err}")))?;

    let url = format!(
        "{}/api/v2/auth/refresh",
        binding.server_url.trim_end_matches('/')
    );
    let response = http
        .post(url)
        .json(&serde_json::json!({ "refresh_token": known.refresh_token }))
        .send()
        .await
        .map_err(|err| AppError::Other(format!("token refresh failed: {err}")))?;

    let status = response.status().as_u16();
    if classify_status(status).is_some() {
        return Err(AppError::Other(format!(
            "token refresh rejected with status {status}"
        )));
    }

    let fresh: AuthTokensResponse = response
        .json()
        .await
        .map_err(|err| AppError::Other(format!("malformed refresh response: {err}")))?;

    let pair = TokenPair {
        access_token: fresh.access_token,
        refresh_token: fresh.refresh_token,
        expires_at: tokens::deadline_from_expires_in(fresh.expires_in),
        username: Some(fresh.user.username),
    };

    // The server has now rotated: the token we presented is dead, and the
    // replacement exists only in this function. If the profile switched
    // while the request was in flight we cannot persist it — its `data.db`
    // is closed — and the pair is lost, leaving that profile to sign in
    // again. Losing it is the right side of the trade: the alternative is
    // writing one account's credentials into another profile's database.
    let pool = state
        .require_profile_pool_for(Some(profile_id))
        .await
        .inspect_err(|_| {
            tracing::warn!(
                profile_id,
                "profile switched during a token refresh; the rotated pair \
                 could not be stored and that profile will need to sign in again"
            );
        })?;
    let mut conn = pool.acquire().await?;
    tokens::write(&mut conn, &pair).await?;

    // The server echoes the device back rather than minting a new one,
    // so this normally matches what we already hold. Adopting it anyway
    // costs nothing and keeps a re-issued device from silently
    // invalidating every subsequent mutation.
    if let RemoteIdentity::Waveflow {
        account_id, cursor, ..
    } = &binding.identity
    {
        if binding.identity.device_id() != Some(fresh.device_id.as_str()) {
            let updated = RemoteBinding {
                identity: RemoteIdentity::Waveflow {
                    account_id: account_id.clone(),
                    device_id: fresh.device_id,
                    cursor: *cursor,
                },
                ..binding.clone()
            };
            binding::write(&mut conn, &updated).await?;
        }
    }

    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_not_a_failure() {
        for status in [200, 201, 204, 299] {
            assert_eq!(classify_status(status), None, "status {status}");
        }
    }

    #[test]
    fn credentials_get_their_own_class_so_a_refresh_can_be_tried() {
        assert_eq!(classify_status(401), Some(FailureKind::Unauthorized));
    }

    #[test]
    fn an_idempotency_conflict_is_permanent() {
        // The server answers 422 both for a malformed payload and for a
        // reused operation id with a different fingerprint. Neither can
        // succeed on retry, so the queue must stop rather than spin.
        assert_eq!(classify_status(422), Some(FailureKind::Permanent));
    }

    #[test]
    fn missing_and_forbidden_are_permanent() {
        assert_eq!(classify_status(403), Some(FailureKind::Permanent));
        assert_eq!(classify_status(404), Some(FailureKind::Permanent));
    }

    #[test]
    fn congestion_and_server_faults_are_worth_retrying() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert_eq!(
                classify_status(status),
                Some(FailureKind::Transient),
                "status {status}"
            );
        }
    }

    #[test]
    fn a_mutation_carries_both_identity_headers() {
        let binding = RemoteBinding {
            server_url: "https://music.example".into(),
            identity: RemoteIdentity::Waveflow {
                account_id: "acc".into(),
                device_id: "dev".into(),
                cursor: 0,
            },
            active_library_id: None,
            bootstrapped_at: None,
        };
        let pair = TokenPair {
            access_token: "wfa_token".into(),
            refresh_token: "wfr_token".into(),
            expires_at: None,
            username: None,
        };
        let client = RemoteClient::from_parts(&binding, &pair).unwrap();

        let request = client
            .mutate(reqwest::Method::PUT, "/api/v2/favorites/track/t1", "op-1")
            .build()
            .unwrap();

        assert_eq!(request.headers()[OPERATION_ID_HEADER], "op-1");
        assert_eq!(request.headers()[DEVICE_ID_HEADER], "dev");
        assert_eq!(
            request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer wfa_token"
        );
        assert_eq!(
            request.url().as_str(),
            "https://music.example/api/v2/favorites/track/t1"
        );
    }

    #[test]
    fn a_read_carries_no_idempotency_headers() {
        let binding = RemoteBinding {
            server_url: "https://music.example/".into(),
            identity: RemoteIdentity::Waveflow {
                account_id: "acc".into(),
                device_id: "dev".into(),
                cursor: 0,
            },
            active_library_id: None,
            bootstrapped_at: None,
        };
        let pair = TokenPair {
            access_token: "wfa_token".into(),
            refresh_token: "wfr_token".into(),
            expires_at: None,
            username: None,
        };
        let client = RemoteClient::from_parts(&binding, &pair).unwrap();
        let request = client.get("/api/v2/sync/snapshot").build().unwrap();

        assert!(request.headers().get(OPERATION_ID_HEADER).is_none());
        assert!(request.headers().get(DEVICE_ID_HEADER).is_none());
        // The trailing slash on the base must not survive into the path.
        assert_eq!(
            request.url().as_str(),
            "https://music.example/api/v2/sync/snapshot"
        );
    }
}
