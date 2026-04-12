//! Last.fm public API client for artist metadata enrichment.
//!
//! Uses the read-only `artist.getInfo` endpoint which requires just an
//! API key as a query parameter — no OAuth, no shared secret, no
//! signing. Rate limit is ~5 requests/second, generous enough for
//! interactive usage.
//!
//! Scope is intentionally narrow: we only fetch artist bios. Images
//! are deprecated on Last.fm (they return empty placeholders since
//! 2020) so we stick with Deezer for those.

use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

const BASE_URL: &str = "http://ws.audioscrobbler.com/2.0/";
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
