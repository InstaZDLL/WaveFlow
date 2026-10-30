//! The MusicBrainz release-group search, as the radar uses it.
//!
//! MusicBrainz asks for two things of a client: a User-Agent that names
//! the application and a way to reach its authors (an anonymous one is
//! answered with `403`), and an average of at most one request a second,
//! a `503` being its answer to a client that goes faster. The pacing is
//! the caller's, which waits on the process-wide
//! [`crate::metadata::musicbrainz::shared_pacer`] before each request;
//! this client says when it was told to slow down, with the delay
//! MusicBrainz asked for.

use std::time::Duration;

use crate::metadata::{default_http, read_bounded, BodyError, MAX_RESPONSE_BYTES};

use super::{lucene_query, parse_release_groups, ArtistQuery, ParsedPage};

const BASE_URL: &str = "https://musicbrainz.org/ws/2";

/// The application, its version and where its authors are, as
/// MusicBrainz requires.
pub const USER_AGENT: &str = concat!(
    "WaveFlow/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/InstaZDLL/WaveFlow )"
);

const TIMEOUT_SECS: u64 = 15;

/// Release groups per answer. Twenty-five recent ones for one artist is
/// already a great deal; the ceiling only stops a prolific one from
/// being cut short.
const PAGE_SIZE: u32 = 50;

/// The longest delay a `Retry-After` is taken at. Past it the scan stops
/// and the next one picks up, rather than sitting idle for minutes.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum MusicBrainzError {
    /// `503` or `429`: too fast, or MusicBrainz is busy. Never retried
    /// in a loop — that is what gets a client blocked.
    #[error("MusicBrainz asked to slow down")]
    RateLimited { retry_after: Option<Duration> },
    #[error("MusicBrainz answered HTTP {status}")]
    Status { status: u16 },
    /// Built through `From`, which strips the URL.
    #[error("request failed: {}", crate::error::describe_http_error(.0))]
    Http(reqwest::Error),
    #[error("the answer exceeds {limit} bytes")]
    TooLarge { limit: usize },
    #[error("the answer is not the expected JSON: {0}")]
    Decode(#[from] serde_json::Error),
}

impl From<reqwest::Error> for MusicBrainzError {
    fn from(err: reqwest::Error) -> Self {
        MusicBrainzError::Http(err.without_url())
    }
}

impl From<BodyError> for MusicBrainzError {
    fn from(err: BodyError) -> Self {
        match err {
            BodyError::Http(err) => MusicBrainzError::Http(err),
            BodyError::TooLarge { limit } => MusicBrainzError::TooLarge { limit },
        }
    }
}

pub struct MusicBrainzClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for MusicBrainzClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicBrainzClient {
    pub fn new() -> Self {
        Self::at(BASE_URL)
    }

    /// The same client, aimed at another address — a test's listener.
    pub fn at(base_url: &str) -> Self {
        Self {
            http: default_http(USER_AGENT, TIMEOUT_SECS),
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// The artist's release groups out on or after `cutoff`
    /// (`YYYY-MM-DD`), with what the answer taught about the artist's id.
    pub async fn recent_release_groups(
        &self,
        artist: &ArtistQuery<'_>,
        cutoff: &str,
    ) -> Result<ParsedPage, MusicBrainzError> {
        let query = lucene_query(artist, cutoff);
        let limit = PAGE_SIZE.to_string();
        let response = self
            .http
            .get(format!("{}/release-group", self.base_url))
            .query(&[
                ("query", query.as_str()),
                ("fmt", "json"),
                ("limit", limit.as_str()),
            ])
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        let status = response.status().as_u16();
        if status == 503 || status == 429 {
            return Err(MusicBrainzError::RateLimited {
                retry_after: retry_after(response.headers()),
            });
        }
        if !(200..300).contains(&status) {
            return Err(MusicBrainzError::Status { status });
        }
        let body = read_bounded(response, MAX_RESPONSE_BYTES).await?;
        Ok(parse_release_groups(&body, artist, cutoff)?)
    }
}

/// A `Retry-After` given in seconds, capped at [`MAX_RETRY_AFTER`]. The
/// HTTP-date form is not one MusicBrainz sends, and reads as none.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

#[cfg(test)]
mod tests;
