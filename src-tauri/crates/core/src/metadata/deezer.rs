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

    /// Search tracks by a free-text query (typically "artist title").
    /// Each hit carries its album cover URLs — used to resolve artwork
    /// for a now-playing Web Radio song parsed from an ICY `StreamTitle`.
    pub async fn search_track(&self, query: &str) -> reqwest::Result<Vec<DeezerTrackHit>> {
        let resp: DeezerSearchResponse<DeezerTrackHit> = self
            .http
            .get(format!("{BASE_URL}/search/track"))
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

    /// Fetch artists Deezer reports as related to the given artist.
    /// Used as a fallback when Last.fm has no API key or returned no
    /// similar artists. Deezer's `/artist/{id}/related` returns a fixed
    /// list ordered by Deezer's own affinity score (no `match` weight
    /// surfaced — callers should treat the order as the ranking).
    pub async fn get_related_artists(
        &self,
        deezer_id: i64,
    ) -> reqwest::Result<Vec<DeezerArtistHit>> {
        let resp: DeezerSearchResponse<DeezerArtistHit> = self
            .http
            .get(format!("{BASE_URL}/artist/{deezer_id}/related"))
            .send()
            .await?
            .json()
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
        // Real world: all sizes share the hash, so a placeholder xl means
        // every size is a placeholder → None.
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
