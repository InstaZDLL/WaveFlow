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
    /// Covers both a malformed payload (`422 validation_error`) and an
    /// idempotency conflict (`409 conflict`). The server separates the
    /// two on purpose — the fixes differ, one is "correct the request"
    /// and the other "mint a new operation id" — but neither is helped
    /// by sending the same bytes again.
    Permanent,
    /// The condition may clear on its own — congestion, a restart, a
    /// flaky link. Worth retrying with backoff.
    Transient,
    /// Credentials were refused. Handled internally by one refresh; if
    /// it reaches the caller, the profile is genuinely signed out.
    Unauthorized,
}

/// Error code for a cursor that precedes the oldest retained event.
///
/// Shares its `409` with [`CODE_CONFLICT`], and the two demand **opposite**
/// reactions — which is precisely why the code, not the status, is what
/// gets tested. See [`RemoteFailure::is_cursor_expired`].
pub const CODE_CURSOR_EXPIRED: &str = "cursor_expired";

/// Error code for a request that collides with existing state — an
/// operation id replayed with a different payload, most of all.
pub const CODE_CONFLICT: &str = "conflict";

/// Classify an HTTP status.
///
/// `408` and `429` sit with the 5xx range rather than with the other
/// 4xx: they describe a moment, not a malformed request.
///
/// `409` is permanent **as a status**, which is right for a conflict:
/// replaying it can never succeed. It is not the whole story for a read,
/// where the same status may carry `cursor_expired` and mean "recover" —
/// so callers branch on the code rather than on this alone.
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
    /// The server's machine-readable code, kept **structured** rather
    /// than folded into the message.
    ///
    /// It has to be: `409` covers both `conflict` and `cursor_expired`,
    /// and reacting to one as the other is destructive in both
    /// directions — treating an expired cursor as a conflict abandons a
    /// write that would have succeeded, and treating a conflict as an
    /// expired cursor throws away a perfectly healthy projection.
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for RemoteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.status, &self.code) {
            (Some(status), Some(code)) => write!(f, "{status} {code}: {}", self.message),
            (Some(status), None) => write!(f, "{status}: {}", self.message),
            (None, _) => write!(f, "{}", self.message),
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
            code: None,
            message: error.to_string(),
        }
    }

    /// The journal no longer reaches back this far: discard the
    /// projection and take a fresh snapshot.
    pub fn is_cursor_expired(&self) -> bool {
        self.code.as_deref() == Some(CODE_CURSOR_EXPIRED)
    }

    /// The request collides with existing state. Permanent, and the fix
    /// is a new operation id — never a retry of this one.
    pub fn is_conflict(&self) -> bool {
        self.code.as_deref() == Some(CODE_CONFLICT)
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

/// What a client needs to refresh its own token and replay a request
/// once when the server answers `401`. Held only by a client built from a
/// live [`AppState`] (via [`RemoteClient::try_build`]); a
/// [`RemoteClient::from_parts`] client — the test constructor — has none
/// and simply surfaces the `401`.
struct RefreshCtx<'a> {
    state: &'a AppState,
    profile_id: i64,
    binding: RemoteBinding,
    pair: TokenPair,
}

/// A client bound to one profile's server and credentials.
///
/// Built per operation rather than held long-term: the credential it
/// carries is a snapshot, and a client kept across a refresh would go on
/// presenting a token that has been rotated away.
pub struct RemoteClient<'a> {
    base_url: String,
    device_id: Option<String>,
    access_token: String,
    http: reqwest::Client,
    /// Present when built from a live profile: lets [`Self::send`] refresh
    /// and replay a single `401` in-band. Absent for the test constructor.
    refresh_ctx: Option<RefreshCtx<'a>>,
}

impl<'a> RemoteClient<'a> {
    /// Build against the active profile's binding and tokens, or
    /// `Ok(None)` when the profile is not bound to a native server.
    ///
    /// Returns `None` — not an error — for an unbound profile, a
    /// signed-out one, or a Subsonic binding: local-only is the normal
    /// case, and a server without a journal has no business here.
    pub async fn try_build(state: &'a AppState) -> AppResult<Option<Self>> {
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

        let mut client = Self::from_parts(&binding, &pair)?;
        client.refresh_ctx = Some(RefreshCtx {
            state,
            profile_id,
            binding,
            pair,
        });
        Ok(Some(client))
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
            refresh_ctx: None,
        })
    }

    /// A read request. No idempotency headers — a GET has nothing to
    /// replay.
    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    /// A plain authenticated request, with no idempotency headers.
    ///
    /// For the endpoints that are not journalled mutations — the
    /// acknowledgement above all. Stamping those with an operation
    /// identifier would advertise a replay guarantee the server does not
    /// implement for them.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .bearer_auth(&self.access_token)
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
            code: None,
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
                code: None,
                message: "offline mode is on".into(),
            });
        }

        let request = builder.build().map_err(RemoteFailure::transport)?;
        // Keep a clone to replay after a refresh if the token is refused.
        // `try_clone` returns `None` only for a streaming body, which none
        // of our requests use.
        let replay = request.try_clone();
        let response = self
            .http
            .execute(request)
            .await
            .map_err(RemoteFailure::transport)?;

        if response.status().as_u16() != 401 {
            return Self::interpret(response).await;
        }

        // A 401 means the access token was refused. `try_build`'s
        // pre-emptive refresh covers the common lapse; this handles the
        // narrow race where the token expired (or was rotated) between that
        // check and the server reading it. Refresh once — single-flighted
        // process-wide — and replay with the fresh token. Anything that
        // stops us (no refresh context, an uncloneable request, a failed
        // refresh) falls through to surfacing the original 401 as
        // `Unauthorized`, and the queue retries later.
        let (Some(ctx), Some(mut replay)) = (self.refresh_ctx.as_ref(), replay) else {
            return Self::interpret(response).await;
        };
        let fresh = match refresh(ctx.state, ctx.profile_id, &ctx.binding, &ctx.pair).await {
            Ok(pair) => pair,
            Err(_) => return Self::interpret(response).await,
        };
        let Ok(bearer) =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", fresh.access_token))
        else {
            return Self::interpret(response).await;
        };
        replay
            .headers_mut()
            .insert(reqwest::header::AUTHORIZATION, bearer);
        let retried = self
            .http
            .execute(replay)
            .await
            .map_err(RemoteFailure::transport)?;
        Self::interpret(retried).await
    }

    /// Turn a completed response into `Ok(response)` or a classified
    /// [`RemoteFailure`], reading the structured error body while it is
    /// still available. The code is kept as a field, not formatted away:
    /// callers branch on it, and a substring match on a message would be a
    /// fragile way to make a destructive decision.
    async fn interpret(
        response: reqwest::Response,
    ) -> Result<reqwest::Response, RemoteFailure> {
        let status = response.status().as_u16();
        match classify_status(status) {
            None => Ok(response),
            Some(kind) => {
                let (code, message) = match response.json::<ErrorBody>().await {
                    Ok(body) => (Some(body.code), body.message),
                    Err(_) => (None, "no error body".to_string()),
                };
                Err(RemoteFailure {
                    kind,
                    status: Some(status),
                    code,
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

        // Somebody may have acted while we waited for the gate. If the
        // stored pair changed, another task already refreshed — adopt its
        // result rather than spend our now-stale token. If it is gone
        // entirely, a concurrent sign-out or forget cleared it, and
        // refreshing now would silently resurrect the credentials the user
        // just dropped — so stop and let the caller treat this as
        // signed-out.
        match tokens::read(&mut conn).await? {
            Some(current) if current.refresh_token != known.refresh_token => {
                return Ok(current);
            }
            Some(_) => {}
            None => {
                return Err(AppError::Other(
                    "signed out while a token refresh was queued".into(),
                ));
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

    // The rotated pair and any device-id correction commit together. A
    // half-write — new tokens stored but the re-issued device left behind —
    // would let every subsequent mutation stamp the wrong device and 401,
    // so the two writes share one transaction.
    let mut tx = pool.begin().await?;
    tokens::write(&mut tx, &pair).await?;

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
            binding::write(&mut tx, &updated).await?;
        }
    }
    tx.commit().await?;

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
    fn a_malformed_payload_and_a_conflict_are_both_permanent() {
        // Different codes, different fixes, same verdict for the queue:
        // sending the same bytes again cannot help.
        assert_eq!(classify_status(422), Some(FailureKind::Permanent));
        assert_eq!(classify_status(409), Some(FailureKind::Permanent));
    }

    fn failure(status: u16, code: &str) -> RemoteFailure {
        RemoteFailure {
            kind: classify_status(status).unwrap(),
            status: Some(status),
            code: Some(code.into()),
            message: "…".into(),
        }
    }

    #[test]
    fn the_two_meanings_of_409_are_told_apart_by_code_not_status() {
        // Confusing them is destructive both ways: an expired cursor
        // read as a conflict abandons a write that would have worked, a
        // conflict read as an expired cursor throws away a healthy
        // projection.
        let expired = failure(409, CODE_CURSOR_EXPIRED);
        assert!(expired.is_cursor_expired());
        assert!(!expired.is_conflict());

        let conflict = failure(409, CODE_CONFLICT);
        assert!(conflict.is_conflict());
        assert!(!conflict.is_cursor_expired());

        assert_eq!(expired.status, conflict.status, "the status cannot decide");
    }

    #[test]
    fn a_failure_without_a_code_claims_neither_meaning() {
        // A body we could not parse must not be guessed into a
        // destructive branch.
        let opaque = RemoteFailure {
            kind: FailureKind::Permanent,
            status: Some(409),
            code: None,
            message: "no error body".into(),
        };
        assert!(!opaque.is_cursor_expired());
        assert!(!opaque.is_conflict());
    }

    #[test]
    fn the_code_shows_up_when_a_failure_is_displayed() {
        assert_eq!(
            failure(409, CODE_CURSOR_EXPIRED).to_string(),
            "409 cursor_expired: …"
        );
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
