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

use super::{
    default_http, read_bounded, require_secure, BaseUrlError, BodyError, LookupError,
    MAX_RESPONSE_BYTES,
};

const BASE_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const USER_AGENT: &str = "WaveFlow/0.1";
const TIMEOUT_SECS: u64 = 5;

// ── API response types ──────────────────────────────────────────────
// Only the fields we actually use — Last.fm returns a lot more but
// serde will silently ignore unknown keys.

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // `message` is captured for diagnostics but unused
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
    base_url: String,
}

impl Default for LastfmClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Last.fm's code for "the artist you supplied could not be found" —
/// the one error that is an answer rather than a failure.
const ERROR_NOT_FOUND: i64 = 6;

/// Reads a read-only method's answer through the size ceiling, and
/// sorts Last.fm's in-band `error` into the two things it can mean.
///
/// Last.fm reports most failures as JSON with an `error` code, sometimes
/// under a failure status and sometimes under 200, so the code decides
/// first. Only a body that names no code — because it does not parse, or
/// parses as nothing in particular — falls back on the status.
async fn read_reply<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
    error_of: impl Fn(&T) -> Option<i64>,
) -> Result<Option<T>, LookupError> {
    let response = request.send().await?;
    let status = response.status();
    let body = read_bounded(response, MAX_RESPONSE_BYTES).await?;
    let reply: T = match serde_json::from_slice(&body) {
        Ok(reply) => reply,
        Err(_) if !status.is_success() => {
            return Err(LookupError::Status {
                status: status.as_u16(),
            });
        }
        Err(err) => return Err(err.into()),
    };
    match error_of(&reply) {
        // A failure status whose body parses but names no error — `{}`
        // under a 503 — is still a failure: read as a reply it would say
        // "no such artist".
        None if !status.is_success() => Err(LookupError::Status {
            status: status.as_u16(),
        }),
        None => Ok(Some(reply)),
        Some(ERROR_NOT_FOUND) => Ok(None),
        Some(code) => Err(LookupError::Refused { code }),
    }
}

impl LastfmClient {
    pub fn new() -> Self {
        Self {
            http: default_http(USER_AGENT, TIMEOUT_SECS),
            base_url: BASE_URL.to_owned(),
        }
    }

    /// A client over `http`, aimed at `base_url` — the API root, the one
    /// that ends in `/2.0/`. Last.fm answers `/2.0` without the slash
    /// with a redirect, which a client that follows none would read as a
    /// failure.
    ///
    /// Refused unless `base_url` is `https` or this machine: every call
    /// carries a key, and the signed ones a session key and a password.
    pub fn with_http(
        http: reqwest::Client,
        base_url: impl Into<String>,
    ) -> Result<Self, BaseUrlError> {
        Ok(Self {
            http,
            base_url: require_secure(base_url.into())?,
        })
    }

    /// Fetch an artist's bio via `artist.getInfo`. Returns `Ok(None)`
    /// when Last.fm reports no match (`error=6`) or the response has
    /// no artist payload; any other error code is a [`LookupError`],
    /// because a refused key or a spent quota is not an artist without a
    /// biography. HTML anchor links in `summary` are stripped before
    /// return so the UI can render plain text.
    pub async fn artist_get_info(
        &self,
        name: &str,
        api_key: &str,
    ) -> Result<Option<LastfmArtistInfo>, LookupError> {
        self.get_info(("artist", name), api_key).await
    }

    /// [`Self::artist_get_info`] keyed on a MusicBrainz artist id rather
    /// than a name. An id has no homonyms, so when the tags carry one it
    /// is the better question to ask.
    pub async fn artist_get_info_by_mbid(
        &self,
        mbid: &str,
        api_key: &str,
    ) -> Result<Option<LastfmArtistInfo>, LookupError> {
        self.get_info(("mbid", mbid), api_key).await
    }

    async fn get_info(
        &self,
        subject: (&str, &str),
        api_key: &str,
    ) -> Result<Option<LastfmArtistInfo>, LookupError> {
        let request = self.http.get(&self.base_url).query(&[
            ("method", "artist.getinfo"),
            subject,
            ("api_key", api_key),
            ("format", "json"),
            ("autocorrect", "1"),
        ]);
        let Some(resp) = read_reply(request, |r: &LastfmArtistGetInfoResponse| r.error).await?
        else {
            return Ok(None);
        };

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

// ── artist.getSimilar ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LastfmSimilarResponse {
    similarartists: Option<LastfmSimilarPayload>,
    #[serde(default)]
    error: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LastfmSimilarPayload {
    artist: Vec<LastfmSimilarArtist>,
}

#[derive(Debug, Deserialize)]
struct LastfmSimilarArtist {
    name: String,
    #[serde(default, rename = "match")]
    match_score: Option<String>,
    #[serde(default)]
    image: Vec<LastfmImage>,
}

#[derive(Debug, Deserialize)]
struct LastfmImage {
    #[serde(default, rename = "#text")]
    url: String,
    /// Last.fm's image sizes: `small`, `medium`, `large`, `extralarge`,
    /// `mega`. We pick the largest non-empty one available.
    #[serde(default)]
    size: String,
}

/// Single similar-artist entry returned to the UI. `match_score` is the
/// 0-1 affinity Last.fm reports — we keep it raw so the frontend can
/// either sort by it or display it as a percentage.
#[derive(Debug, Clone)]
pub struct SimilarArtist {
    pub name: String,
    pub match_score: f32,
    pub picture_url: Option<String>,
}

impl LastfmClient {
    /// Fetch up to `limit` similar artists via `artist.getSimilar`.
    /// Returns an empty vec when Last.fm reports no match (`error=6`)
    /// or the response carries no `similarartists` payload — callers
    /// can then fall back to Deezer's `/artist/{id}/related`. Any other
    /// error code is a [`LookupError`], so a refused key falls back too
    /// instead of being remembered as an artist with no neighbours.
    pub async fn artist_get_similar(
        &self,
        name: &str,
        api_key: &str,
        limit: u32,
    ) -> Result<Vec<SimilarArtist>, LookupError> {
        let limit_str = limit.to_string();
        let request = self.http.get(&self.base_url).query(&[
            ("method", "artist.getsimilar"),
            ("artist", name),
            ("api_key", api_key),
            ("format", "json"),
            ("autocorrect", "1"),
            ("limit", limit_str.as_str()),
        ]);
        let Some(resp) = read_reply(request, |r: &LastfmSimilarResponse| r.error).await? else {
            return Ok(Vec::new());
        };
        let Some(payload) = resp.similarartists else {
            return Ok(Vec::new());
        };

        let out = payload
            .artist
            .into_iter()
            .map(|a| {
                let match_score = a
                    .match_score
                    .as_deref()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let picture_url = pick_largest_image(&a.image);
                SimilarArtist {
                    name: a.name,
                    match_score,
                    picture_url,
                }
            })
            .collect();
        Ok(out)
    }
}

/// Pick the largest non-empty Last.fm image URL. Last.fm images are
/// notoriously the same generic star placeholder for any artist without
/// a uploaded picture, so callers should still prefer Deezer covers
/// when available. Returned in priority `mega` > `extralarge` > `large`.
fn pick_largest_image(images: &[LastfmImage]) -> Option<String> {
    for size in &["mega", "extralarge", "large"] {
        if let Some(img) = images.iter().find(|i| i.size == *size && !i.url.is_empty()) {
            return Some(img.url.clone());
        }
    }
    None
}

/// Last.fm serves one md5-named star asset for every artist without an
/// uploaded picture (their artist-image API was retired in 2019). The CDN
/// host has changed over the years but the asset hash is stable, so a
/// substring match on it is the reliable "no real photo" test. Callers
/// that fall back to a raw Last.fm URL filter it out so the UI shows its
/// initial-letter avatar instead of the grey star (#406).
pub fn is_lastfm_star_placeholder(url: &str) -> bool {
    url.contains("2a96cbd8b46e442fc41c2b86b821562f")
}

// ── Signed authentication ───────────────────────────────────────────

/// Errors that can come out of the signed-method surface. Network +
/// HTTP failures are wrapped, plus Last.fm's structured `error` /
/// `message` payload that comes back with a 200 OK.
#[derive(Debug, thiserror::Error)]
pub enum LastfmError {
    /// Built through `From`, which strips the URL, as every transport
    /// error in this module is.
    /// Its message carries the cause chain — the refusal, the DNS
    /// failure, the timeout — see [`crate::error::describe_http_error`].
    #[error("network: {}", crate::error::describe_http_error(.0))]
    Http(reqwest::Error),
    #[error("Last.fm API error {code}: {message}")]
    Api { code: i64, message: String },
    #[error("Last.fm's answer exceeds {limit} bytes")]
    TooLarge { limit: usize },
}

impl From<reqwest::Error> for LastfmError {
    fn from(err: reqwest::Error) -> Self {
        LastfmError::Http(err.without_url())
    }
}

impl From<BodyError> for LastfmError {
    fn from(err: BodyError) -> Self {
        match err {
            BodyError::Http(err) => LastfmError::Http(err),
            BodyError::TooLarge { limit } => LastfmError::TooLarge { limit },
        }
    }
}

/// A signed method's answer, read through the same ceiling as every other.
async fn read_signed_body(response: reqwest::Response) -> Result<String, LastfmError> {
    let body = read_bounded(response, MAX_RESPONSE_BYTES).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
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
            .post(&self.base_url)
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

        let body = read_signed_body(resp).await?;
        if let Ok(err) = serde_json::from_str::<LastfmErrorBody>(&body) {
            return Err(LastfmError::Api {
                code: err.error,
                message: err.message,
            });
        }
        let parsed: MobileSessionResponse =
            serde_json::from_str(&body).map_err(|e| LastfmError::Api {
                code: -1,
                // Not the body: a reply that fails to decode may still
                // carry a session key, and this message is logged.
                message: format!("decode mobile session: {e}"),
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

        let resp = self.http.post(&self.base_url).form(&form).send().await?;
        let body = read_signed_body(resp).await?;
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

        let resp = self.http.post(&self.base_url).form(&form).send().await?;
        let body = read_signed_body(resp).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_star_placeholder_across_cdn_hosts() {
        assert!(is_lastfm_star_placeholder(
            "https://lastfm.freetls.fastly.net/i/u/300x300/2a96cbd8b46e442fc41c2b86b821562f.png"
        ));
        assert!(is_lastfm_star_placeholder(
            "https://lastfm-img2.akamaized.net/i/u/174s/2a96cbd8b46e442fc41c2b86b821562f.png"
        ));
    }

    #[test]
    fn accepts_a_real_lastfm_picture() {
        assert!(!is_lastfm_star_placeholder(
            "https://lastfm.freetls.fastly.net/i/u/300x300/abc123def456abc123def456abc12345.png"
        ));
    }

    use crate::metadata::test_server::{serve, serve_json, strict_http, Reply};

    /// A secret and a password made at run time. Handed to a signed call,
    /// a literal is a hard-coded credential to CodeQL whatever the test is
    /// about, and these tests are about the body and the error, not the
    /// values.
    fn throwaway_credentials() -> (String, String) {
        let nonce = std::process::id();
        (nonce.to_string(), nonce.wrapping_add(1).to_string())
    }

    fn client_for(base_url: &str) -> LastfmClient {
        LastfmClient::with_http(strict_http(), format!("{base_url}/2.0/")).unwrap()
    }

    #[tokio::test]
    async fn a_biography_is_read_through_an_injected_client_and_address() {
        let provider = serve_json(
            r#"{"artist":{"name":"Björk","bio":{"summary":"Icelandic. <a href=\"https://www.last.fm/music/Bj%C3%B6rk\">Read more on Last.fm</a>","content":"Born in Reykjavík."}}}"#,
        )
        .await;

        let info = client_for(&provider.base_url)
            .artist_get_info("Björk", "the-key")
            .await
            .unwrap()
            .expect("a biography");

        assert_eq!(info.bio_full.as_deref(), Some("Born in Reykjavík."));
        let line = provider.request_line(0);
        assert!(line.starts_with("GET /2.0/?"), "{line}");
        assert!(line.contains("method=artist.getinfo"), "{line}");
        assert!(line.contains("api_key=the-key"), "{line}");
    }

    #[tokio::test]
    async fn an_unknown_artist_is_an_answer() {
        let provider =
            serve_json(r#"{"error":6,"message":"The artist you supplied could not be found"}"#)
                .await;

        let info = client_for(&provider.base_url)
            .artist_get_info("Nobody At All", "the-key")
            .await
            .expect("not found is not a failure");

        assert!(info.is_none());
    }

    /// The reason this module's read methods stopped answering `Ok` for
    /// every error code: a refused key is a failure to ask, and a caller
    /// that caches answers would otherwise remember it as an artist
    /// without a biography.
    #[tokio::test]
    async fn a_refused_key_is_a_failure_and_not_an_absence() {
        let provider = serve(Reply {
            status: "403 Forbidden",
            headers: &[],
            body: r#"{"error":10,"message":"Invalid API key - You must be granted a valid key by last.fm"}"#,
            announce_length: true,
        })
        .await;

        let error = client_for(&provider.base_url)
            .artist_get_info("Björk", "a-bad-key")
            .await
            .expect_err("a refused key must not read as an absence");

        assert!(
            matches!(error, LookupError::Refused { code: 10 }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_spent_quota_under_a_success_status_is_still_a_failure() {
        let provider = serve_json(r#"{"error":29,"message":"Rate limit exceeded"}"#).await;

        let error = client_for(&provider.base_url)
            .artist_get_similar("Björk", "the-key", 10)
            .await
            .expect_err("a throttled call must not read as no neighbours");

        assert!(
            matches!(error, LookupError::Refused { code: 29 }),
            "{error:?}"
        );
    }

    #[tokio::test]
    /// The redirect stays inside the origin on purpose: the desktop's own
    /// client follows those, so only the client handed to `with_http` —
    /// which follows none — can be the reason this one is refused. A
    /// redirect to another origin would be refused by either, and could
    /// not tell them apart.
    async fn a_redirect_is_not_followed_by_a_client_that_follows_none() {
        let provider = serve(Reply {
            status: "302 Found",
            headers: &["location: /2.0/moved"],
            body: "",
            announce_length: true,
        })
        .await;

        let error = client_for(&provider.base_url)
            .artist_get_info("Björk", "the-key")
            .await
            .expect_err("a redirect is not an answer");

        assert!(
            matches!(error, LookupError::Status { status: 302 }),
            "{error:?}"
        );
        assert_eq!(provider.hits(), 1, "the redirect was followed");
    }

    #[tokio::test]
    async fn an_answer_over_the_ceiling_is_refused_even_unannounced() {
        let padding = "x".repeat(MAX_RESPONSE_BYTES);
        let body = format!(r#"{{"artist":{{"name":"{padding}"}}}}"#);
        let provider = serve(Reply {
            status: "200 OK",
            headers: &[],
            body: &body,
            announce_length: false,
        })
        .await;

        let error = client_for(&provider.base_url)
            .artist_get_info("Björk", "the-key")
            .await
            .expect_err("an oversized answer must not be read whole");

        assert!(matches!(error, LookupError::TooLarge { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn a_musicbrainz_id_is_asked_as_one() {
        let provider = serve_json(r#"{"artist":{"name":"Björk"}}"#).await;

        client_for(&provider.base_url)
            .artist_get_info_by_mbid("87c5dedd-371d-4a53-9f7f-80522fb7f3cb", "the-key")
            .await
            .unwrap();

        let line = provider.request_line(0);
        assert!(
            line.contains("mbid=87c5dedd-371d-4a53-9f7f-80522fb7f3cb"),
            "{line}"
        );
        assert!(!line.contains("artist="), "{line}");
    }

    /// The key travels in the query string, and `reqwest` prints the URL
    /// of a failed request — so an unredacted error would hand the key to
    /// whatever logs it.
    #[tokio::test]
    async fn a_transport_error_does_not_carry_the_key() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let error = LastfmClient::with_http(strict_http(), format!("http://{address}/2.0/"))
            .unwrap()
            .artist_get_info("Björk", "the-secret-key")
            .await
            .expect_err("nothing is listening");

        assert!(matches!(error, LookupError::Http(_)), "{error:?}");
        assert!(!format!("{error}").contains("the-secret-key"), "{error}");
        assert!(
            !format!("{error:?}").contains("the-secret-key"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_failure_status_with_an_empty_object_is_a_failure() {
        let provider = serve(Reply {
            status: "503 Service Unavailable",
            headers: &[],
            body: "{}",
            announce_length: true,
        })
        .await;

        let error = client_for(&provider.base_url)
            .artist_get_info("Björk", "the-key")
            .await
            .expect_err("an outage must not read as an unknown artist");

        assert!(
            matches!(error, LookupError::Status { status: 503 }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_signed_answer_over_the_ceiling_is_refused_even_unannounced() {
        let (secret, password) = throwaway_credentials();
        let padding = "x".repeat(MAX_RESPONSE_BYTES);
        let body = format!(r#"{{"session":{{"name":"{padding}","key":"k"}}}}"#);
        let provider = serve(Reply {
            status: "200 OK",
            headers: &[],
            body: &body,
            announce_length: false,
        })
        .await;

        let error = client_for(&provider.base_url)
            .auth_get_mobile_session("the-key", &secret, "someone", &password)
            .await
            .expect_err("an oversized answer must not be read whole");

        assert!(matches!(error, LastfmError::TooLarge { .. }), "{error:?}");
    }

    /// A reply that fails to decode can still carry the session key, which
    /// is good until revoked — and the error it becomes is logged.
    #[tokio::test]
    async fn a_session_reply_that_fails_to_decode_does_not_carry_its_key() {
        let (secret, password) = throwaway_credentials();
        let provider = serve_json(r#"{"session":{"key":"the-session-key"}}"#).await;

        let error = client_for(&provider.base_url)
            .auth_get_mobile_session("the-key", &secret, "someone", &password)
            .await
            .expect_err("a session without a name does not decode");

        assert!(!format!("{error}").contains("the-session-key"), "{error}");
        assert!(
            !format!("{error:?}").contains("the-session-key"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_signed_transport_error_does_not_carry_its_url() {
        let (secret, password) = throwaway_credentials();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let client = LastfmClient::with_http(
            strict_http(),
            format!("http://{address}/a-path-worth-hiding/2.0/"),
        )
        .unwrap();

        let error = client
            .auth_get_mobile_session("the-key", &secret, "someone", &password)
            .await
            .expect_err("nothing is listening");

        assert!(matches!(error, LastfmError::Http(_)), "{error:?}");
        assert!(
            !format!("{error:?}").contains("a-path-worth-hiding"),
            "{error:?}"
        );
    }

    #[test]
    fn a_plaintext_address_off_this_machine_is_refused() {
        let refused = LastfmClient::with_http(strict_http(), "http://ws.audioscrobbler.com/2.0/");
        assert!(matches!(refused, Err(BaseUrlError::Plaintext { .. })));
    }
}
