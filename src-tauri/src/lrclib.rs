//! LRCLIB public API client for fetching synchronized lyrics.
//!
//! LRCLIB (<https://lrclib.net>) is a free, anonymous-access lyrics
//! database with no API key, no rate limit advertised, and a permissive
//! Public Domain license. The `/api/get` endpoint matches by artist +
//! track + album + duration so we can be confident the lyrics line up
//! with the user's local file.
//!
//! Both `plainLyrics` (text) and `syncedLyrics` (LRC format with
//! `[mm:ss.xx]` timestamps) are returned when available.

use serde::Deserialize;

const BASE_URL: &str = "https://lrclib.net/api/get";
const USER_AGENT: &str = "WaveFlow/0.1 (https://github.com/InstaZDLL/waveflow)";
const TIMEOUT_SECS: u64 = 6;

/// Subset of the LRCLIB response we actually need.
#[derive(Debug, Clone, Deserialize)]
pub struct LrclibResponse {
    pub instrumental: Option<bool>,
    #[serde(rename = "plainLyrics")]
    pub plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics")]
    pub synced_lyrics: Option<String>,
}

pub struct LrclibClient {
    http: reqwest::Client,
}

impl LrclibClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// Fetch lyrics by exact track metadata. Returns `Ok(None)` for
    /// 404 (no match in LRCLIB) so the caller doesn't have to
    /// distinguish "not found" from "network error".
    ///
    /// `duration_seconds` should come from `track.duration_ms / 1000`
    /// rounded to the nearest second — LRCLIB matches with a small
    /// tolerance so don't worry about exact precision.
    pub async fn get(
        &self,
        artist_name: &str,
        track_name: &str,
        album_name: Option<&str>,
        duration_seconds: u64,
    ) -> reqwest::Result<Option<LrclibResponse>> {
        let mut params: Vec<(&str, String)> = vec![
            ("artist_name", artist_name.to_string()),
            ("track_name", track_name.to_string()),
            ("duration", duration_seconds.to_string()),
        ];
        if let Some(album) = album_name {
            if !album.is_empty() {
                params.push(("album_name", album.to_string()));
            }
        }

        let resp = self.http.get(BASE_URL).query(&params).send().await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let body: LrclibResponse = resp.error_for_status()?.json().await?;
        Ok(Some(body))
    }
}
