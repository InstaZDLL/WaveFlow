//! Deezer public API client for metadata enrichment.
//!
//! All endpoints used here are **unauthenticated** — no API key or OAuth
//! token required. The client wraps a single `reqwest::Client` with a 5 s
//! timeout and a `WaveFlow/0.1` user-agent.
//!
//! Rate limit: Deezer allows ~50 requests per 5 seconds per IP.
//! For interactive usage (user clicks an album/artist) this is more than
//! enough — no local rate-limiter is needed in v1.

use serde::Deserialize;

const BASE_URL: &str = "https://api.deezer.com";
const USER_AGENT: &str = "WaveFlow/0.1";
const TIMEOUT_SECS: u64 = 5;

/// Thin wrapper around `reqwest::Client` pre-configured for Deezer.
/// `Clone` is cheap — `reqwest::Client` is `Arc`-backed — and lets
/// callers stamp the client into each future of a `buffer_unordered`
/// stream without hitting the closure-lifetime HRTB wall.
#[derive(Clone)]
pub struct DeezerClient {
    http: reqwest::Client,
}

// ── API response types ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeezerSearchResponse<T> {
    pub data: Vec<T>,
}

/// Deezer's in-band error object.
///
/// The API answers a refusal with **HTTP 200** and this in the body
/// instead of the payload — `{"error":{"type":"Exception","message":
/// "Quota limit exceeded","code":4}}`. Every field is optional because
/// the shape isn't documented and varies by endpoint.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DeezerApiError {
    /// Deezer's exception class (`Exception`, `DataException`, …).
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub message: Option<String>,
    pub code: Option<i64>,
}

impl std::fmt::Display for DeezerApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.message, self.code) {
            (Some(msg), Some(code)) => write!(f, "{msg} (code {code})"),
            (Some(msg), None) => write!(f, "{msg}"),
            (None, Some(code)) => write!(f, "code {code}"),
            (None, None) => f.write_str("no reason given"),
        }
    }
}

impl DeezerApiError {
    /// The rate limiter, which is the one an operator can act on: it
    /// means "come back in a moment", not "this album doesn't exist".
    pub fn is_quota_exceeded(&self) -> bool {
        self.code == Some(4)
            || self
                .message
                .as_deref()
                .is_some_and(|m| m.to_ascii_lowercase().contains("quota"))
    }
}

/// What can go wrong on a Deezer call.
///
/// The `Api` arm exists because the transport succeeded: without it,
/// an in-band error deserialized as a missing `data` field and surfaced
/// as a decode error, which every caller logs and treats as "no
/// results". A throttled client and an artist Deezer has never heard of
/// looked exactly alike (#406 was diagnosed through that fog).
#[derive(Debug, thiserror::Error)]
pub enum DeezerError {
    #[error("deezer request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("deezer refused the request: {0}")]
    Api(DeezerApiError),
    /// Throttling that arrived as a status code rather than in the body.
    /// Deezer usually answers 200 with an `error` object, but the edge
    /// in front of it can rate-limit on its own — and that response
    /// carries no JSON to read, so it needs its own arm rather than a
    /// decode failure.
    #[error("deezer rate-limited the request (HTTP 429)")]
    RateLimited,
}

impl DeezerError {
    /// True when Deezer said "slow down". Lets a caller tell a real
    /// empty result from a throttled one without matching on strings.
    pub fn is_quota_exceeded(&self) -> bool {
        match self {
            DeezerError::Api(err) => err.is_quota_exceeded(),
            DeezerError::RateLimited => true,
            DeezerError::Http(_) => false,
        }
    }
}

pub type DeezerResult<T> = Result<T, DeezerError>;

/// Either the payload or the in-band error, decided by which one the
/// body actually parses as. `Error` is listed first so a body that
/// carries both (never seen, but the shape allows it) is read as the
/// refusal it is.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DeezerReply<T> {
    Error { error: DeezerApiError },
    Payload(T),
}

impl<T> DeezerReply<T> {
    fn into_result(self) -> DeezerResult<T> {
        match self {
            DeezerReply::Payload(value) => Ok(value),
            DeezerReply::Error { error } => Err(DeezerError::Api(error)),
        }
    }
}

// Smaller/medium variants and counts come from the API but we only
// consume the larger images plus a few aggregates downstream — keep
// them deserialized so the struct stays a faithful mirror of the
// response payload.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DeezerArtistHit {
    pub id: i64,
    pub name: String,
    pub picture_small: Option<String>,
    pub picture_medium: Option<String>,
    pub picture_big: Option<String>,
    pub picture_xl: Option<String>,
    pub nb_album: Option<i64>,
    pub nb_fan: Option<i64>,
}

/// Deezer serves a grey-silhouette placeholder when an artist has no real
/// picture: every size resolves to the same CDN path but with an *empty*
/// image hash — `…/images/artist//500x500…` (double slash) — or the md5
/// of the empty string. Caching one of those as a real image is the
/// "similar artist shows no photo" half of #406, so callers filter them
/// out at every point a Deezer picture URL is accepted.
pub fn is_placeholder_artist_picture(url: &str) -> bool {
    url.contains("/artist//") || url.contains("/artist/d41d8cd98f00b204e9800998ecf8427e/")
}

impl DeezerArtistHit {
    /// Highest-quality *real* picture URL, largest first, skipping
    /// Deezer's empty-hash placeholder (#406). `None` when the artist has
    /// no genuine image — every size shares the one hash, so a placeholder
    /// in `picture_xl` means all the others are placeholders too.
    pub fn best_picture(&self) -> Option<String> {
        [
            &self.picture_xl,
            &self.picture_big,
            &self.picture_medium,
            &self.picture_small,
        ]
        .into_iter()
        .flatten()
        .find(|u| !is_placeholder_artist_picture(u))
        .cloned()
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DeezerAlbumHit {
    pub id: i64,
    pub title: String,
    pub cover_small: Option<String>,
    pub cover_medium: Option<String>,
    pub cover_big: Option<String>,
    pub cover_xl: Option<String>,
    pub nb_tracks: Option<i64>,
    pub label: Option<String>,
    pub release_date: Option<String>,
    /// Present on `/search/album` results; absent on `/album/{id}`.
    pub artist: Option<DeezerAlbumArtist>,
}

#[derive(Debug, Deserialize)]
pub struct DeezerAlbumArtist {
    pub name: String,
}

// `/search/track` hits. We only consume the album cover downstream (to
// resolve artwork for a now-playing Web Radio song parsed from ICY
// `StreamTitle`) but keep the id/title/artist deserialized so the struct
// mirrors the response.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DeezerTrackHit {
    pub id: i64,
    pub title: String,
    pub artist: Option<DeezerAlbumArtist>,
    pub album: Option<DeezerTrackAlbum>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DeezerTrackAlbum {
    pub id: i64,
    pub title: String,
    pub cover_small: Option<String>,
    pub cover_medium: Option<String>,
    pub cover_big: Option<String>,
    pub cover_xl: Option<String>,
}

// ── Client implementation ───────────────────────────────────────────

impl Default for DeezerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DeezerClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// Send `request` and read the body as `T`, turning Deezer's in-band
    /// error object into an `Err` instead of a decode failure.
    ///
    /// The status is checked first because `send()` does not: a 429 from
    /// the edge carries no JSON at all, and letting it reach `json()`
    /// turned throttling back into the decode error this arm exists to
    /// stop producing.
    async fn fetch<T: serde::de::DeserializeOwned>(
        request: reqwest::RequestBuilder,
    ) -> DeezerResult<T> {
        let response = request.send().await?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(DeezerError::RateLimited);
        }
        response
            .error_for_status()?
            .json::<DeezerReply<T>>()
            .await?
            .into_result()
    }

    /// Search artists by name. Returns up to 25 hits (Deezer default).
    pub async fn search_artist(&self, name: &str) -> DeezerResult<Vec<DeezerArtistHit>> {
        let resp: DeezerSearchResponse<DeezerArtistHit> = Self::fetch(
            self.http
                .get(format!("{BASE_URL}/search/artist"))
                .query(&[("q", name)]),
        )
        .await?;
        Ok(resp.data)
    }

    /// Fetch a single artist by Deezer ID.
    pub async fn get_artist(&self, deezer_id: i64) -> DeezerResult<DeezerArtistHit> {
        Self::fetch(self.http.get(format!("{BASE_URL}/artist/{deezer_id}"))).await
    }

    /// Search albums by a free-text query (typically "album title artist name").
    pub async fn search_album(&self, query: &str) -> DeezerResult<Vec<DeezerAlbumHit>> {
        let resp: DeezerSearchResponse<DeezerAlbumHit> = Self::fetch(
            self.http
                .get(format!("{BASE_URL}/search/album"))
                .query(&[("q", query)]),
        )
        .await?;
        Ok(resp.data)
    }

    /// Search tracks by a free-text query (typically "artist title").
    /// Each hit carries its album cover URLs — used to resolve artwork
    /// for a now-playing Web Radio song parsed from an ICY `StreamTitle`.
    pub async fn search_track(&self, query: &str) -> DeezerResult<Vec<DeezerTrackHit>> {
        let resp: DeezerSearchResponse<DeezerTrackHit> = Self::fetch(
            self.http
                .get(format!("{BASE_URL}/search/track"))
                .query(&[("q", query)]),
        )
        .await?;
        Ok(resp.data)
    }

    /// Fetch a single album by Deezer ID.
    pub async fn get_album(&self, deezer_id: i64) -> DeezerResult<DeezerAlbumHit> {
        Self::fetch(self.http.get(format!("{BASE_URL}/album/{deezer_id}"))).await
    }

    /// Fetch artists Deezer reports as related to the given artist.
    /// Used as a fallback when Last.fm has no API key or returned no
    /// similar artists. Deezer's `/artist/{id}/related` returns a fixed
    /// list ordered by Deezer's own affinity score (no `match` weight
    /// surfaced — callers should treat the order as the ranking).
    pub async fn get_related_artists(&self, deezer_id: i64) -> DeezerResult<Vec<DeezerArtistHit>> {
        let resp: DeezerSearchResponse<DeezerArtistHit> = Self::fetch(
            self.http
                .get(format!("{BASE_URL}/artist/{deezer_id}/related")),
        )
        .await?;
        Ok(resp.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(xl: Option<&str>, big: Option<&str>) -> DeezerArtistHit {
        DeezerArtistHit {
            id: 1,
            name: "Test".into(),
            picture_small: None,
            picture_medium: None,
            picture_big: big.map(str::to_string),
            picture_xl: xl.map(str::to_string),
            nb_album: None,
            nb_fan: None,
        }
    }

    fn decode_artists(body: &str) -> DeezerResult<Vec<DeezerArtistHit>> {
        serde_json::from_str::<DeezerReply<DeezerSearchResponse<DeezerArtistHit>>>(body)
            .expect("body should parse as one arm or the other")
            .into_result()
            .map(|r| r.data)
    }

    #[test]
    fn an_in_band_error_is_an_error_and_not_an_empty_result() {
        // Served with HTTP 200. Before this arm existed the missing
        // `data` field made it a decode failure, which every caller
        // logged and treated as "Deezer knows nothing about this".
        let err = decode_artists(
            r#"{"error":{"type":"Exception","message":"Quota limit exceeded","code":4}}"#,
        )
        .expect_err("an error body must not read as a result");
        assert!(err.is_quota_exceeded(), "{err}");
        assert_eq!(
            err.to_string(),
            "deezer refused the request: Quota limit exceeded (code 4)"
        );
    }

    #[test]
    fn a_genuinely_empty_result_stays_a_result() {
        // The case the error arm must not swallow: Deezer answered, and
        // the answer is that it has nothing.
        assert_eq!(decode_artists(r#"{"data":[]}"#).unwrap().len(), 0);
    }

    #[test]
    fn a_populated_result_still_decodes() {
        let hits = decode_artists(r#"{"data":[{"id":27,"name":"Daft Punk"}]}"#).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Daft Punk");
    }

    #[test]
    fn a_body_carrying_both_reads_as_the_refusal() {
        // `#[serde(untagged)]` tries the variants in declaration order
        // and `Error` ignores unknown fields, so a body with `data` *and*
        // `error` resolves to the refusal. That is the precedence we
        // want — a partial payload next to a refusal is still a refusal
        // — and this test is what keeps a variant reorder from silently
        // flipping it.
        let err = decode_artists(
            r#"{"data":[{"id":27,"name":"Daft Punk"}],"error":{"message":"Quota limit exceeded","code":4}}"#,
        )
        .expect_err("a body carrying an error must not read as a result");
        assert!(err.is_quota_exceeded(), "{err}");
    }

    #[test]
    fn a_status_level_rate_limit_is_a_quota_refusal_too() {
        assert!(DeezerError::RateLimited.is_quota_exceeded());
    }

    #[test]
    fn a_non_quota_refusal_is_reported_as_itself() {
        let err = decode_artists(r#"{"error":{"type":"DataException","message":"no data"}}"#)
            .expect_err("still an error");
        assert!(!err.is_quota_exceeded());
        assert_eq!(err.to_string(), "deezer refused the request: no data");
    }

    #[test]
    fn detects_empty_hash_placeholder() {
        assert!(is_placeholder_artist_picture(
            "https://e-cdns-images.dzcdn.net/images/artist//500x500-000000-80-0-0.jpg"
        ));
        assert!(is_placeholder_artist_picture(
            "https://e-cdns-images.dzcdn.net/images/artist/d41d8cd98f00b204e9800998ecf8427e/500x500.jpg"
        ));
    }

    #[test]
    fn accepts_a_real_hash() {
        assert!(!is_placeholder_artist_picture(
            "https://e-cdns-images.dzcdn.net/images/artist/f2bc007e9133c946ac3c3907ddc5d2ea/500x500.jpg"
        ));
    }

    #[test]
    fn best_picture_skips_a_placeholder_and_takes_the_next_real_size() {
        // Exercises the `.find()` skip: a placeholder in the largest slot
        // must not short-circuit — the next real size wins.
        let h = hit(
            Some("https://e-cdns-images.dzcdn.net/images/artist//1000x1000.jpg"),
            Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/500x500.jpg"),
        );
        assert_eq!(
            h.best_picture().as_deref(),
            Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/500x500.jpg")
        );
    }

    #[test]
    fn best_picture_is_none_when_every_size_is_a_placeholder() {
        // The real-world shape: all sizes share the one empty hash.
        let ph = hit(
            Some("https://e-cdns-images.dzcdn.net/images/artist//1000x1000.jpg"),
            Some("https://e-cdns-images.dzcdn.net/images/artist//500x500.jpg"),
        );
        assert_eq!(ph.best_picture(), None);
    }

    #[test]
    fn best_picture_prefers_the_largest_real_size() {
        let h = hit(
            Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/1000x1000.jpg"),
            Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/500x500.jpg"),
        );
        assert_eq!(
            h.best_picture().as_deref(),
            Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/1000x1000.jpg")
        );
    }

    #[test]
    fn best_picture_is_none_when_no_sizes_present() {
        assert_eq!(hit(None, None).best_picture(), None);
    }
}
