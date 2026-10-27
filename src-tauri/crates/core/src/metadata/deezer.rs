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

use crate::metadata::{default_http, read_bounded, BodyError, MAX_RESPONSE_BYTES};

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
    base_url: String,
}

// ── API response types ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeezerSearchResponse<T> {
    pub data: Vec<T>,
    /// Link to the following page, absent on the last one. Deezer sends
    /// it on every list endpoint; the search paths here read one page
    /// on purpose, so it is only consulted where a short answer would
    /// be a wrong answer.
    #[serde(default)]
    pub next: Option<String>,
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
    /// Built through `From`, which strips the URL: it carries the search
    /// query, and a caller that logs this error must not log that.
    /// Its message carries the cause chain — the refusal, the DNS
    /// failure, the timeout — see [`crate::error::describe_http_error`].
    #[error("deezer request failed: {}", crate::error::describe_http_error(.0))]
    Http(reqwest::Error),
    #[error("deezer refused the request: {0}")]
    Api(DeezerApiError),
    /// Throttling that arrived as a status code rather than in the body.
    /// Deezer usually answers 200 with an `error` object, but the edge
    /// in front of it can rate-limit on its own — and that response
    /// carries no JSON to read, so it needs its own arm rather than a
    /// decode failure.
    #[error("deezer rate-limited the request (HTTP 429)")]
    RateLimited,
    #[error("deezer's answer exceeds {limit} bytes")]
    TooLarge { limit: usize },
    #[error("deezer's answer is not the expected JSON: {0}")]
    Decode(#[from] serde_json::Error),
}

impl From<reqwest::Error> for DeezerError {
    fn from(err: reqwest::Error) -> Self {
        DeezerError::Http(err.without_url())
    }
}

impl From<BodyError> for DeezerError {
    fn from(err: BodyError) -> Self {
        match err {
            BodyError::Http(err) => DeezerError::Http(err),
            BodyError::TooLarge { limit } => DeezerError::TooLarge { limit },
        }
    }
}

impl DeezerError {
    /// True when Deezer said "slow down". Lets a caller tell a real
    /// empty result from a throttled one without matching on strings.
    pub fn is_quota_exceeded(&self) -> bool {
        match self {
            DeezerError::Api(err) => err.is_quota_exceeded(),
            DeezerError::RateLimited => true,
            DeezerError::Http(_) | DeezerError::TooLarge { .. } | DeezerError::Decode(_) => false,
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

#[derive(Debug, Clone, Deserialize)]
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

/// One track of an album listing.
///
/// `duration` is in **seconds** — the only place in this file where a
/// duration is not milliseconds, and the matcher compares against
/// `track.duration_ms`, so the conversion has to happen at the border
/// rather than being noticed later as "every duration disagrees".
#[derive(Debug, Clone, Deserialize)]
pub struct DeezerAlbumTrack {
    pub id: i64,
    pub title: String,
    pub duration: Option<i64>,
    /// Position on its disc, which is what a track number means on a
    /// multi-disc release.
    pub track_position: Option<i64>,
    pub disk_number: Option<i64>,
    pub artist: Option<DeezerAlbumArtist>,
}

impl DeezerAlbumTrack {
    /// The track's length in milliseconds, or `None` when the
    /// catalogue did not give one.
    pub fn duration_ms(&self) -> Option<i64> {
        self.duration.map(|s| s * 1000)
    }
}

/// A public playlist's header.
#[derive(Debug, Clone, Deserialize)]
pub struct DeezerPlaylist {
    pub id: i64,
    pub title: String,
    /// How many tracks Deezer says the playlist holds.
    pub nb_tracks: Option<i64>,
}

/// The album a playlist track belongs to, as far as an import needs it.
#[derive(Debug, Clone, Deserialize)]
pub struct DeezerPlaylistAlbum {
    pub title: Option<String>,
}

/// One track of a playlist listing.
///
/// `duration` is in **seconds**, as on [`DeezerAlbumTrack`]. `isrc` comes
/// with every entry of this listing, unlike the album one, and is what the
/// importer matches first.
#[derive(Debug, Clone, Deserialize)]
pub struct DeezerPlaylistTrack {
    pub id: i64,
    pub title: String,
    pub duration: Option<i64>,
    pub isrc: Option<String>,
    pub artist: Option<DeezerAlbumArtist>,
    pub album: Option<DeezerPlaylistAlbum>,
}

impl DeezerPlaylistTrack {
    pub fn duration_ms(&self) -> Option<i64> {
        self.duration.filter(|s| *s > 0).map(|s| s * 1000)
    }
}

// ── Client implementation ───────────────────────────────────────────

impl Default for DeezerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DeezerClient {
    pub fn new() -> Self {
        Self::with_http(default_http(USER_AGENT, TIMEOUT_SECS), BASE_URL)
    }

    /// A client over `http`, aimed at `base_url` — the API root, with no
    /// trailing slash.
    pub fn with_http(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
        }
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
        let response = response.error_for_status()?;
        let body = read_bounded(response, MAX_RESPONSE_BYTES).await?;
        serde_json::from_slice::<DeezerReply<T>>(&body)?.into_result()
    }

    /// Search artists by name. Returns up to 25 hits (Deezer default).
    pub async fn search_artist(&self, name: &str) -> DeezerResult<Vec<DeezerArtistHit>> {
        let resp: DeezerSearchResponse<DeezerArtistHit> = Self::fetch(
            self.http
                .get(format!("{}/search/artist", self.base_url))
                .query(&[("q", name)]),
        )
        .await?;
        Ok(resp.data)
    }

    /// Fetch a single artist by Deezer ID.
    pub async fn get_artist(&self, deezer_id: i64) -> DeezerResult<DeezerArtistHit> {
        Self::fetch(
            self.http
                .get(format!("{}/artist/{deezer_id}", self.base_url)),
        )
        .await
    }

    /// Search albums by a free-text query (typically "album title artist name").
    pub async fn search_album(&self, query: &str) -> DeezerResult<Vec<DeezerAlbumHit>> {
        let resp: DeezerSearchResponse<DeezerAlbumHit> = Self::fetch(
            self.http
                .get(format!("{}/search/album", self.base_url))
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
                .get(format!("{}/search/track", self.base_url))
                .query(&[("q", query)]),
        )
        .await?;
        Ok(resp.data)
    }

    /// Fetch a single album by Deezer ID.
    pub async fn get_album(&self, deezer_id: i64) -> DeezerResult<DeezerAlbumHit> {
        Self::fetch(
            self.http
                .get(format!("{}/album/{deezer_id}", self.base_url)),
        )
        .await
    }

    /// The tracks of an album, in the catalogue's own order, **every
    /// page of them**.
    ///
    /// `/album/{id}/tracks` rather than the `tracks` field of
    /// `/album/{id}`: the embedded list is capped. But so is this one —
    /// it is a paged list like every other Deezer collection, and a box
    /// set read as one page comes back short with nothing in the
    /// response to say it was cut. The pages are walked until the API
    /// stops offering a next one.
    ///
    /// The following page is requested by `index`, computed here,
    /// rather than by following the `next` URL the response carries: it
    /// keeps every request this client makes one this code built, and
    /// it costs a line.
    pub async fn get_album_tracks(&self, deezer_id: i64) -> DeezerResult<Vec<DeezerAlbumTrack>> {
        /// Deezer's own maximum page size for a collection.
        const PAGE: usize = 100;
        /// Ten thousand tracks. A real release is three orders of
        /// magnitude below this; the cap is here so a catalogue that
        /// keeps saying "there is more" cannot spin forever.
        const MAX_PAGES: usize = 100;

        let mut out: Vec<DeezerAlbumTrack> = Vec::new();
        let mut index = 0usize;
        for _ in 0..MAX_PAGES {
            let resp: DeezerSearchResponse<DeezerAlbumTrack> = Self::fetch(
                self.http
                    .get(format!("{}/album/{deezer_id}/tracks", self.base_url))
                    .query(&[("index", index.to_string()), ("limit", PAGE.to_string())]),
            )
            .await?;
            let received = resp.data.len();
            out.extend(resp.data);
            // `next` is the authority on whether there is more — a page
            // shorter than asked for is not the end, and treating it as
            // one truncated the listing. The offset advances by what
            // actually arrived rather than by the page size, or a short
            // page would leave a hole. An empty page stops the walk
            // whatever `next` claims, since nothing else would.
            if resp.next.is_none() || received == 0 {
                break;
            }
            index += received;
        }
        Ok(out)
    }

    /// Fetch a public playlist's header. A private or deleted playlist is
    /// refused in-band, which arrives as [`DeezerError::Api`].
    pub async fn get_playlist(&self, deezer_id: u64) -> DeezerResult<DeezerPlaylist> {
        Self::fetch(
            self.http
                .get(format!("{}/playlist/{deezer_id}", self.base_url)),
        )
        .await
    }

    /// One page of a public playlist's tracks, from `index` on, and whether
    /// there is more. A page rather than the whole listing: a playlist runs
    /// to thousands of tracks, and the caller paces the pages to stay under
    /// the anonymous rate limit, with the timer of its own runtime.
    pub async fn get_playlist_tracks_page(
        &self,
        deezer_id: u64,
        index: usize,
    ) -> DeezerResult<(Vec<DeezerPlaylistTrack>, bool)> {
        /// Deezer's own maximum page size for a collection.
        const PAGE: usize = 100;
        let resp: DeezerSearchResponse<DeezerPlaylistTrack> = Self::fetch(
            self.http
                .get(format!("{}/playlist/{deezer_id}/tracks", self.base_url))
                .query(&[("index", index.to_string()), ("limit", PAGE.to_string())]),
        )
        .await?;
        // `next` is the authority on whether there is more; an empty page
        // ends the walk whatever it claims.
        let more = resp.next.is_some() && !resp.data.is_empty();
        Ok((resp.data, more))
    }

    /// Fetch artists Deezer reports as related to the given artist.
    /// Used as a fallback when Last.fm has no API key or returned no
    /// similar artists. Deezer's `/artist/{id}/related` returns a fixed
    /// list ordered by Deezer's own affinity score (no `match` weight
    /// surfaced — callers should treat the order as the ranking).
    pub async fn get_related_artists(&self, deezer_id: i64) -> DeezerResult<Vec<DeezerArtistHit>> {
        let resp: DeezerSearchResponse<DeezerArtistHit> = Self::fetch(
            self.http
                .get(format!("{}/artist/{deezer_id}/related", self.base_url)),
        )
        .await?;
        Ok(resp.data)
    }
}

#[cfg(test)]
mod tests;
