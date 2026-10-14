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

use serde::Deserialize;
use serde_json::{json, Value};

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
    #[error("network: {0}")]
    Http(#[from] reqwest::Error),
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
}

impl Default for ListenBrainzClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenBrainzClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// The user the token belongs to, or `None` when ListenBrainz says the
    /// token is not valid.
    pub async fn validate_token(&self, token: &str) -> Result<Option<String>, ListenBrainzError> {
        let resp = self
            .http
            .get(format!("{BASE_URL}/validate-token"))
            .header("Authorization", format!("Token {token}"))
            .send()
            .await?;
        let status = resp.status();
        if status.as_u16() == 401 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(classify(
                status.as_u16(),
                resp.text().await.unwrap_or_default(),
            ));
        }
        let body: ValidateTokenResponse = resp.json().await?;
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
            .post(format!("{BASE_URL}/submit-listens"))
            .header("Authorization", format!("Token {token}"))
            .json(&submit_body(kind, listen))
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        Err(classify(
            status.as_u16(),
            resp.text().await.unwrap_or_default(),
        ))
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
}
