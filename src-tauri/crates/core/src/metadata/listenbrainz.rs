//! ListenBrainz client: check a user token, submit listens, announce the
//! track playing now.
//!
//! The open counterpart to Last.fm's scrobbling. Simpler to authenticate:
//! the user pastes the token from their ListenBrainz profile page, and
//! every call carries it as `Authorization: Token <token>` — no app key,
//! no signature, no session exchange.
//!
//! Network only. The caller decides when to call (offline mode, the
//! settling delay before "playing now", the retry schedule) — the same
//! split as [`super::lastfm`].
//!
//! It follows the module's two rules for a client that carries a
//! credential (#880): redirects stay inside the origin first asked, since
//! every request carries the user's token, and every answer — the error
//! bodies included — is read through [`read_bounded`]'s ceiling.

use serde::Deserialize;
use serde_json::{json, Value};

use super::{default_http, read_bounded, BodyError, MAX_RESPONSE_BYTES};

const BASE_URL: &str = "https://api.listenbrainz.org/1";
/// ListenBrainz asks submitting clients to identify themselves.
const USER_AGENT: &str = concat!("WaveFlow/", env!("CARGO_PKG_VERSION"));
const TIMEOUT_SECS: u64 = 10;
/// Named in every listen's `additional_info`, as the API documents.
const CLIENT_NAME: &str = "WaveFlow";

/// One listen, as the API wants it.
#[derive(Debug, Clone)]
pub struct Listen<'a> {
    pub artist: &'a str,
    pub track: &'a str,
    pub release: Option<&'a str>,
    pub duration_ms: Option<i64>,
    pub track_number: Option<i64>,
    /// When the listen started, in Unix seconds. `None` for "playing
    /// now", which the API refuses to see a timestamp on.
    pub listened_at: Option<i64>,
}

/// The three kinds of submission the endpoint takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenType {
    /// One finished listen.
    Single,
    /// The track playing now: shown on the profile, not stored.
    PlayingNow,
}

impl ListenType {
    fn as_str(self) -> &'static str {
        match self {
            ListenType::Single => "single",
            ListenType::PlayingNow => "playing_now",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ListenBrainzError {
    /// Built through `From`, which strips the URL, as every transport
    /// error in this module is.
    #[error("network: {0}")]
    Http(reqwest::Error),
    /// The answer outgrew [`MAX_RESPONSE_BYTES`] and was not read.
    #[error("ListenBrainz's answer exceeds {limit} bytes")]
    TooLarge { limit: usize },
    /// The token check answered something that is not its JSON.
    #[error("ListenBrainz's answer is not the expected JSON: {0}")]
    Decode(#[from] serde_json::Error),
    /// The token is wrong or was revoked: every later call would fail the
    /// same way until the user pastes a new one.
    #[error("ListenBrainz rejected the token")]
    Unauthorized,
    /// The listen itself was refused (malformed, missing a field). Sending
    /// it again cannot help.
    #[error("ListenBrainz refused the listen ({status}): {message}")]
    Rejected { status: u16, message: String },
    /// Rate limited or a server error: worth trying again later.
    #[error("ListenBrainz unavailable ({status}): {message}")]
    Unavailable { status: u16, message: String },
}

impl From<reqwest::Error> for ListenBrainzError {
    fn from(err: reqwest::Error) -> Self {
        ListenBrainzError::Http(err.without_url())
    }
}

impl From<BodyError> for ListenBrainzError {
    fn from(err: BodyError) -> Self {
        match err {
            BodyError::Http(err) => ListenBrainzError::Http(err),
            BodyError::TooLarge { limit } => ListenBrainzError::TooLarge { limit },
        }
    }
}

impl ListenBrainzError {
    /// Whether retrying the same listen is pointless.
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }
}

#[derive(Deserialize)]
struct ValidateTokenResponse {
    #[serde(default)]
    valid: bool,
    user_name: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: Option<String>,
}

pub struct ListenBrainzClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for ListenBrainzClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenBrainzClient {
    pub fn new() -> Self {
        Self {
            http: default_http(USER_AGENT, TIMEOUT_SECS),
            base_url: BASE_URL.to_owned(),
        }
    }

    /// The desktop's client, aimed at a test's listener.
    #[cfg(test)]
    fn at(base_url: &str) -> Self {
        Self {
            http: default_http(USER_AGENT, TIMEOUT_SECS),
            base_url: base_url.to_owned(),
        }
    }

    /// The user the token belongs to, or `None` when ListenBrainz says the
    /// token is not valid.
    pub async fn validate_token(&self, token: &str) -> Result<Option<String>, ListenBrainzError> {
        let resp = self
            .http
            .get(format!("{}/validate-token", self.base_url))
            .header("Authorization", format!("Token {token}"))
            .send()
            .await?;
        let status = resp.status();
        if status.as_u16() == 401 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(classify(status.as_u16(), error_body(resp).await));
        }
        let body: ValidateTokenResponse =
            serde_json::from_slice(&read_bounded(resp, MAX_RESPONSE_BYTES).await?)?;
        Ok(body.valid.then_some(body.user_name).flatten())
    }

    /// Submit one listen, or announce one as playing now.
    pub async fn submit(
        &self,
        token: &str,
        kind: ListenType,
        listen: &Listen<'_>,
    ) -> Result<(), ListenBrainzError> {
        let resp = self
            .http
            .post(format!("{}/submit-listens", self.base_url))
            .header("Authorization", format!("Token {token}"))
            .json(&submit_body(kind, listen))
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        Err(classify(status.as_u16(), error_body(resp).await))
    }
}

/// A failure's body, for its message only: read through the same ceiling
/// as an answer, and empty when it cannot be read — the status alone
/// already says what to do.
async fn error_body(resp: reqwest::Response) -> String {
    match read_bounded(resp, MAX_RESPONSE_BYTES).await {
        Ok(body) => String::from_utf8_lossy(&body).into_owned(),
        Err(_) => String::new(),
    }
}

/// Sort a failed response into what the caller can do about it.
fn classify(status: u16, body: String) -> ListenBrainzError {
    let message = serde_json::from_str::<ErrorBody>(&body)
        .ok()
        .and_then(|b| b.error)
        .unwrap_or(body);
    match status {
        401 => ListenBrainzError::Unauthorized,
        // 429 is a 4xx that means "later", not "never".
        429 => ListenBrainzError::Unavailable { status, message },
        400..=499 => ListenBrainzError::Rejected { status, message },
        _ => ListenBrainzError::Unavailable { status, message },
    }
}

/// The JSON body of a `submit-listens` call. Separate so it can be tested
/// without a server: the shape is the whole contract.
pub fn submit_body(kind: ListenType, listen: &Listen<'_>) -> Value {
    let mut additional = serde_json::Map::new();
    additional.insert("media_player".into(), json!(CLIENT_NAME));
    additional.insert("submission_client".into(), json!(CLIENT_NAME));
    additional.insert(
        "submission_client_version".into(),
        json!(env!("CARGO_PKG_VERSION")),
    );
    if let Some(ms) = listen.duration_ms.filter(|ms| *ms > 0) {
        additional.insert("duration_ms".into(), json!(ms));
    }
    if let Some(n) = listen.track_number.filter(|n| *n > 0) {
        additional.insert("tracknumber".into(), json!(n));
    }

    let mut metadata = serde_json::Map::new();
    metadata.insert("artist_name".into(), json!(listen.artist));
    metadata.insert("track_name".into(), json!(listen.track));
    if let Some(release) = listen.release.filter(|r| !r.trim().is_empty()) {
        metadata.insert("release_name".into(), json!(release));
    }
    metadata.insert("additional_info".into(), Value::Object(additional));

    let mut item = serde_json::Map::new();
    // "playing now" must not carry a timestamp: the API refuses it.
    if kind == ListenType::Single {
        if let Some(at) = listen.listened_at {
            item.insert("listened_at".into(), json!(at));
        }
    }
    item.insert("track_metadata".into(), Value::Object(metadata));

    json!({ "listen_type": kind.as_str(), "payload": [Value::Object(item)] })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listen() -> Listen<'static> {
        Listen {
            artist: "Artist",
            track: "Track",
            release: Some("Album"),
            duration_ms: Some(200_000),
            track_number: Some(3),
            listened_at: Some(1_700_000_000),
        }
    }

    #[test]
    fn a_single_listen_carries_its_time_and_metadata() {
        let body = submit_body(ListenType::Single, &listen());
        assert_eq!(body["listen_type"], "single");
        let item = &body["payload"][0];
        assert_eq!(item["listened_at"], 1_700_000_000);
        let meta = &item["track_metadata"];
        assert_eq!(meta["artist_name"], "Artist");
        assert_eq!(meta["track_name"], "Track");
        assert_eq!(meta["release_name"], "Album");
        assert_eq!(meta["additional_info"]["duration_ms"], 200_000);
        assert_eq!(meta["additional_info"]["tracknumber"], 3);
        assert_eq!(meta["additional_info"]["submission_client"], "WaveFlow");
    }

    /// The API refuses a "playing now" with a timestamp.
    #[test]
    fn playing_now_has_no_timestamp() {
        let body = submit_body(ListenType::PlayingNow, &listen());
        assert_eq!(body["listen_type"], "playing_now");
        assert!(body["payload"][0].get("listened_at").is_none());
    }

    /// Unknown fields are left out rather than sent empty or as zero.
    #[test]
    fn missing_fields_are_omitted() {
        let mut l = listen();
        l.release = Some("  ");
        l.duration_ms = Some(0);
        l.track_number = None;
        let body = submit_body(ListenType::Single, &l);
        let meta = &body["payload"][0]["track_metadata"];
        assert!(meta.get("release_name").is_none());
        assert!(meta["additional_info"].get("duration_ms").is_none());
        assert!(meta["additional_info"].get("tracknumber").is_none());
    }

    #[test]
    fn failures_sort_into_what_can_be_done() {
        assert!(matches!(
            classify(401, String::new()),
            ListenBrainzError::Unauthorized
        ));
        assert!(classify(400, r#"{"error":"bad"}"#.into()).is_permanent());
        assert!(!classify(429, String::new()).is_permanent());
        assert!(!classify(503, String::new()).is_permanent());
        match classify(400, r#"{"code":400,"error":"Missing key"}"#.into()) {
            ListenBrainzError::Rejected { message, .. } => assert_eq!(message, "Missing key"),
            other => panic!("{other:?}"),
        }
    }

    use crate::metadata::test_server::{serve, serve_json, Reply};

    /// The token rides every request, so a redirect out of the origin is
    /// not followed: the other host is never contacted.
    #[tokio::test]
    async fn a_redirect_out_of_the_origin_is_not_followed() {
        let elsewhere = serve_json(r#"{"valid":true,"user_name":"someone"}"#).await;
        let location = format!("location: {}/validate-token", elsewhere.base_url);
        let asked = serve(Reply {
            status: "307 Temporary Redirect",
            headers: &[&location],
            body: "",
            announce_length: true,
        })
        .await;

        let result = ListenBrainzClient::at(&asked.base_url)
            .validate_token("the-token")
            .await;

        assert!(result.is_err(), "{result:?}");
        assert_eq!(elsewhere.hits(), 0, "the token was sent elsewhere");
    }

    /// An answer over the ceiling is refused, not read whole, even when it
    /// announces no length.
    #[tokio::test]
    async fn an_oversized_answer_is_refused() {
        let padding = "x".repeat(MAX_RESPONSE_BYTES);
        let body = format!(r#"{{"valid":true,"user_name":"{padding}"}}"#);
        let provider = serve(Reply {
            status: "200 OK",
            headers: &[],
            body: &body,
            announce_length: false,
        })
        .await;

        let error = ListenBrainzClient::at(&provider.base_url)
            .validate_token("the-token")
            .await
            .expect_err("an oversized answer must not be read whole");

        assert!(
            matches!(error, ListenBrainzError::TooLarge { .. }),
            "{error:?}"
        );
    }

    /// An error body is read through the ceiling too; past it the status
    /// still decides, and the message is simply empty.
    #[tokio::test]
    async fn an_oversized_error_body_still_classifies_by_status() {
        let body = "x".repeat(MAX_RESPONSE_BYTES + 1);
        let provider = serve(Reply {
            status: "503 Service Unavailable",
            headers: &[],
            body: &body,
            announce_length: false,
        })
        .await;

        let error = ListenBrainzClient::at(&provider.base_url)
            .submit("the-token", ListenType::Single, &listen())
            .await
            .expect_err("a 503 is a failure");

        assert!(
            matches!(
                &error,
                ListenBrainzError::Unavailable { status: 503, message } if message.is_empty()
            ),
            "{error:?}"
        );
    }
}
