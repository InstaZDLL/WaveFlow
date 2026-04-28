//! Last.fm API client.
//!
//! Two surfaces:
//!
//! 1. **Read-only `artist.getInfo`** — needs just an API key as a query
//!    parameter, used to enrich artist bios. Rate limit ~5 req/s.
//!
//! 2. **Signed methods** — `auth.getMobileSession`, `track.scrobble`,
//!    `track.updateNowPlaying`. These need both an API key and the
//!    matching shared secret; every request body is signed by sorting
//!    its params, concatenating `keyvalue` pairs, suffixing the secret
//!    and md5'ing the result. The session key obtained from
//!    `auth.getMobileSession` is then included as `sk` on subsequent
//!    calls. Rate limit ~5 req/s for scrobbles.

use std::sync::LazyLock;

use md5::{Digest, Md5};
use regex::Regex;
use serde::Deserialize;

const BASE_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const USER_AGENT: &str = "WaveFlow/0.1";
const TIMEOUT_SECS: u64 = 5;

// ── API response types ──────────────────────────────────────────────
// Only the fields we actually use — Last.fm returns a lot more but
// serde will silently ignore unknown keys.

#[derive(Debug, Deserialize)]
struct LastfmArtistGetInfoResponse {
    artist: Option<LastfmArtistPayload>,
    #[serde(default)]
    error: Option<i64>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LastfmArtistPayload {
    name: String,
    bio: Option<LastfmBio>,
}

#[derive(Debug, Deserialize)]
struct LastfmBio {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// Cleaned artist info returned to callers.
#[derive(Debug, Clone)]
pub struct LastfmArtistInfo {
    #[allow(dead_code)]
    pub name: String,
    pub bio_summary: Option<String>,
    pub bio_full: Option<String>,
}

// ── Client ──────────────────────────────────────────────────────────

pub struct LastfmClient {
    http: reqwest::Client,
}

impl LastfmClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// Fetch an artist's bio via `artist.getInfo`. Returns `Ok(None)`
    /// when Last.fm reports no match (`error=6`) or the response has
    /// no artist payload. HTML anchor links in `summary` are stripped
    /// before return so the UI can render plain text.
    pub async fn artist_get_info(
        &self,
        name: &str,
        api_key: &str,
    ) -> reqwest::Result<Option<LastfmArtistInfo>> {
        let resp: LastfmArtistGetInfoResponse = self
            .http
            .get(BASE_URL)
            .query(&[
                ("method", "artist.getinfo"),
                ("artist", name),
                ("api_key", api_key),
                ("format", "json"),
                ("autocorrect", "1"),
            ])
            .send()
            .await?
            .json()
            .await?;

        if resp.error.is_some() {
            // error=6 = "The artist you supplied could not be found".
            // Any other error also surfaces as an empty enrichment so
            // the caller doesn't need to differentiate.
            return Ok(None);
        }

        let Some(artist) = resp.artist else {
            return Ok(None);
        };

        let bio_summary = artist
            .bio
            .as_ref()
            .and_then(|b| b.summary.as_deref())
            .map(strip_html_tags)
            .filter(|s| !s.is_empty());
        let bio_full = artist
            .bio
            .as_ref()
            .and_then(|b| b.content.as_deref())
            .map(strip_html_tags)
            .filter(|s| !s.is_empty());

        Ok(Some(LastfmArtistInfo {
            name: artist.name,
            bio_summary,
            bio_full,
        }))
    }
}

// ── Signed authentication ───────────────────────────────────────────

/// Errors that can come out of the signed-method surface. Network +
/// HTTP failures are wrapped, plus Last.fm's structured `error` /
/// `message` payload that comes back with a 200 OK.
#[derive(Debug, thiserror::Error)]
pub enum LastfmError {
    #[error("network: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Last.fm API error {code}: {message}")]
    Api { code: i64, message: String },
}

/// Compute the API signature for a Last.fm signed request: sort the
/// parameter pairs by key, concatenate `key1value1key2value2…`,
/// append the shared secret, md5, hex-encode lowercase. Standard
/// behaviour documented at <https://www.last.fm/api/authspec>.
fn sign(params: &[(&str, &str)], secret: &str) -> String {
    let mut sorted: Vec<(&str, &str)> = params.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut hasher = Md5::new();
    for (k, v) in &sorted {
        hasher.update(k.as_bytes());
        hasher.update(v.as_bytes());
    }
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

#[derive(Debug, Deserialize)]
struct LastfmErrorBody {
    error: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct MobileSessionResponse {
    session: Option<MobileSession>,
}

#[derive(Debug, Deserialize)]
struct MobileSession {
    name: String,
    key: String,
}

/// Outcome of `auth.getMobileSession`. `username` may differ from the
/// raw input thanks to autocorrect ("foo" → "Foo") so we return what
/// Last.fm thinks the canonical handle is.
#[derive(Debug, Clone)]
pub struct LastfmSession {
    pub username: String,
    pub session_key: String,
}

impl LastfmClient {
    /// Trade username + password for a long-lived session key via the
    /// `auth.getMobileSession` endpoint. Last.fm requires HTTPS for
    /// this method (which we already use). The returned `session_key`
    /// goes into `auth_credential.token_encrypted` and is sent as the
    /// `sk` parameter on every subsequent signed call.
    pub async fn auth_get_mobile_session(
        &self,
        api_key: &str,
        api_secret: &str,
        username: &str,
        password: &str,
    ) -> Result<LastfmSession, LastfmError> {
        let params = [
            ("api_key", api_key),
            ("method", "auth.getMobileSession"),
            ("password", password),
            ("username", username),
        ];
        let api_sig = sign(&params, api_secret);

        let resp = self
            .http
            .post(BASE_URL)
            .form(&[
                ("api_key", api_key),
                ("method", "auth.getMobileSession"),
                ("password", password),
                ("username", username),
                ("api_sig", &api_sig),
                ("format", "json"),
            ])
            .send()
            .await?;

        let body = resp.text().await?;
        if let Ok(err) = serde_json::from_str::<LastfmErrorBody>(&body) {
            return Err(LastfmError::Api {
                code: err.error,
                message: err.message,
            });
        }
        let parsed: MobileSessionResponse = serde_json::from_str(&body).map_err(|e| {
            LastfmError::Api {
                code: -1,
                message: format!("decode mobile session: {e}; body={body}"),
            }
        })?;
        let Some(session) = parsed.session else {
            return Err(LastfmError::Api {
                code: -1,
                message: "missing session in response".into(),
            });
        };
        Ok(LastfmSession {
            username: session.name,
            session_key: session.key,
        })
    }

    /// Submit a single scrobble. Caller decides whether the listen
    /// qualifies (Last.fm rule: track ≥ 30 s and listened ≥ half its
    /// duration or 4 minutes); this call just signs and POSTs.
    ///
    /// `played_at` is the unix-seconds timestamp at which the listen
    /// started — Last.fm will reject anything older than two weeks
    /// or in the future.
    #[allow(clippy::too_many_arguments)]
    pub async fn scrobble(
        &self,
        api_key: &str,
        api_secret: &str,
        session_key: &str,
        artist: &str,
        track: &str,
        album: Option<&str>,
        track_number: Option<i64>,
        duration_s: Option<i64>,
        played_at_unix_s: i64,
    ) -> Result<(), LastfmError> {
        let played_at = played_at_unix_s.to_string();
        let track_number_str = track_number.map(|n| n.to_string());
        let duration_str = duration_s.map(|n| n.to_string());

        let mut params: Vec<(&str, &str)> = vec![
            ("api_key", api_key),
            ("artist", artist),
            ("method", "track.scrobble"),
            ("sk", session_key),
            ("timestamp", &played_at),
            ("track", track),
        ];
        if let Some(a) = album {
            params.push(("album", a));
        }
        if let Some(ref n) = track_number_str {
            params.push(("trackNumber", n));
        }
        if let Some(ref d) = duration_str {
            params.push(("duration", d));
        }
        let api_sig = sign(&params, api_secret);

        let mut form = params.clone();
        form.push(("api_sig", &api_sig));
        form.push(("format", "json"));

        let resp = self.http.post(BASE_URL).form(&form).send().await?;
        let body = resp.text().await?;
        if let Ok(err) = serde_json::from_str::<LastfmErrorBody>(&body) {
            return Err(LastfmError::Api {
                code: err.error,
                message: err.message,
            });
        }
        Ok(())
    }

    /// Tell Last.fm which track is currently playing. Best-effort, no
    /// retry — if it fails, the user will just miss the "now playing"
    /// indicator on their profile until the next track. The actual
    /// scrobble (which is what matters for stats) goes through the
    /// queued [`scrobble`] path.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_now_playing(
        &self,
        api_key: &str,
        api_secret: &str,
        session_key: &str,
        artist: &str,
        track: &str,
        album: Option<&str>,
        duration_s: Option<i64>,
    ) -> Result<(), LastfmError> {
        let duration_str = duration_s.map(|n| n.to_string());

        let mut params: Vec<(&str, &str)> = vec![
            ("api_key", api_key),
            ("artist", artist),
            ("method", "track.updateNowPlaying"),
            ("sk", session_key),
            ("track", track),
        ];
        if let Some(a) = album {
            params.push(("album", a));
        }
        if let Some(ref d) = duration_str {
            params.push(("duration", d));
        }
        let api_sig = sign(&params, api_secret);

        let mut form = params.clone();
        form.push(("api_sig", &api_sig));
        form.push(("format", "json"));

        let resp = self.http.post(BASE_URL).form(&form).send().await?;
        let body = resp.text().await?;
        if let Ok(err) = serde_json::from_str::<LastfmErrorBody>(&body) {
            return Err(LastfmError::Api {
                code: err.error,
                message: err.message,
            });
        }
        Ok(())
    }
}

/// Last.fm error codes that mean "stop retrying, the queued item will
/// never succeed". From <https://www.last.fm/api/errorcodes>.
pub fn is_permanent_error(code: i64) -> bool {
    matches!(
        code,
        // 6 = invalid parameters; 7 = invalid resource;
        // 9 = invalid session key (re-auth needed — caller should
        //     drop credentials, but the queued item is still dead);
        // 10 = invalid api key; 13 = invalid signature.
        6 | 7 | 9 | 10 | 13
    )
}

// ── HTML cleanup ────────────────────────────────────────────────────

static HTML_ANCHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<a\b[^>]*>.*?</a>").expect("valid regex"));
static HTML_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").expect("valid regex"));

/// Strip the "Read more on Last.fm" anchor that trails every bio,
/// then any residual HTML tags. Collapse runs of whitespace and trim.
fn strip_html_tags(input: &str) -> String {
    let no_anchors = HTML_ANCHOR_RE.replace_all(input, "");
    let no_tags = HTML_TAG_RE.replace_all(&no_anchors, "");
    no_tags
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}
