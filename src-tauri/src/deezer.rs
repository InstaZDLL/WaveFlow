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
pub struct DeezerClient {
    http: reqwest::Client,
}

// ── API response types ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeezerSearchResponse<T> {
    pub data: Vec<T>,
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

// ── Client implementation ───────────────────────────────────────────

impl DeezerClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// Search artists by name. Returns up to 25 hits (Deezer default).
    pub async fn search_artist(&self, name: &str) -> reqwest::Result<Vec<DeezerArtistHit>> {
        let resp: DeezerSearchResponse<DeezerArtistHit> = self
            .http
            .get(format!("{BASE_URL}/search/artist"))
            .query(&[("q", name)])
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.data)
    }

    /// Fetch a single artist by Deezer ID.
    pub async fn get_artist(&self, deezer_id: i64) -> reqwest::Result<DeezerArtistHit> {
        self.http
            .get(format!("{BASE_URL}/artist/{deezer_id}"))
            .send()
            .await?
            .json()
            .await
    }

    /// Search albums by a free-text query (typically "album title artist name").
    pub async fn search_album(&self, query: &str) -> reqwest::Result<Vec<DeezerAlbumHit>> {
        let resp: DeezerSearchResponse<DeezerAlbumHit> = self
            .http
            .get(format!("{BASE_URL}/search/album"))
            .query(&[("q", query)])
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.data)
    }

    /// Fetch a single album by Deezer ID.
    pub async fn get_album(&self, deezer_id: i64) -> reqwest::Result<DeezerAlbumHit> {
        self.http
            .get(format!("{BASE_URL}/album/{deezer_id}"))
            .send()
            .await?
            .json()
            .await
    }
}
