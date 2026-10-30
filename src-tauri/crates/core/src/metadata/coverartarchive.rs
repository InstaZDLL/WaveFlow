//! Cover Art Archive: the front cover of a MusicBrainz release.
//!
//! The archive answers an image request with a redirect to the Internet
//! Archive, which stores the files — `coverartarchive.org` →
//! `archive.org/download/…` → one of its `*.us.archive.org` mirrors. The
//! same-origin rule the other clients follow ([`super::default_http`])
//! would stop at the first hop, so this client has a rule of its own:
//! follow, over https only, and only to those two domains. Nothing it
//! sends is secret, but a cover fetch is not a reason to let a redirect
//! point the app anywhere.
//!
//! Requests are paced with MusicBrainz's [`Pacer`]: the archive asks
//! for the same restraint, and the engine never needs both at once.

use std::sync::Arc;
use std::time::Duration;

use super::musicbrainz::{is_mbid, user_agent, Pacer};
use super::{read_bounded, LookupError};

const BASE_URL: &str = "https://coverartarchive.org";
const TIMEOUT_SECS: u64 = 30;
const MAX_REDIRECTS: usize = 5;
/// How long every later request waits after the archive said "slow down".
const RATE_LIMIT_BACK_OFF: Duration = Duration::from_secs(5);

/// The largest image read. The 500-pixel thumbnail asked for is a few
/// tens of kilobytes; a full-size scan can reach several megabytes,
/// which is what an archive without that thumbnail sends instead.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Which entity's front cover to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverOf {
    /// This exact release.
    Release,
    /// The release group: the cover the archive picked to stand for the
    /// album across its pressings.
    ReleaseGroup,
}

impl CoverOf {
    fn path(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::ReleaseGroup => "release-group",
        }
    }
}

#[derive(Clone)]
pub struct CoverArtArchiveClient {
    http: reqwest::Client,
    base_url: String,
    pacer: Arc<Pacer>,
}

impl CoverArtArchiveClient {
    pub fn new(app_version: &str, pacer: Arc<Pacer>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(user_agent(app_version))
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .redirect(archive_only())
            .build()
            .expect("failed to build reqwest client");
        Self {
            http,
            base_url: BASE_URL.to_owned(),
            pacer,
        }
    }

    pub fn with_http(
        http: reqwest::Client,
        base_url: impl Into<String>,
        pacer: Arc<Pacer>,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            pacer,
        }
    }

    /// The front cover's bytes, at about 500 pixels. `Ok(None)` when the
    /// archive has no front cover for it, or `mbid` is not an identifier.
    pub async fn front(&self, of: CoverOf, mbid: &str) -> Result<Option<Vec<u8>>, LookupError> {
        if !is_mbid(mbid) {
            return Ok(None);
        }
        self.pacer.wait().await;
        let url = format!("{}/{}/{mbid}/front-500", self.base_url, of.path());
        let response = self.http.get(&url).send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            // The archive's rate limiter: the shared pacer slows down for
            // MusicBrainz too, which is what sharing it is for. Not
            // retried here — the cover is picked up by the next pass.
            if matches!(status.as_u16(), 429 | 503) {
                self.pacer.back_off(RATE_LIMIT_BACK_OFF);
            }
            return Err(LookupError::Status {
                status: status.as_u16(),
            });
        }
        Ok(Some(read_bounded(response, MAX_IMAGE_BYTES).await?))
    }
}

/// Follow redirects to the archive's own hosts, over https.
fn archive_only() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        if is_archive_url(attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// `https` to `coverartarchive.org`, `archive.org`, or a subdomain of
/// either.
pub(crate) fn is_archive_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    ["coverartarchive.org", "archive.org"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_archives_hosts_over_https_are_followed() {
        let ok = |s: &str| is_archive_url(&reqwest::Url::parse(s).unwrap());
        assert!(ok("https://archive.org/download/mbid-x/x-500.jpg"));
        assert!(ok("https://ia800505.us.archive.org/1/items/x.jpg"));
        assert!(ok("https://coverartarchive.org/release/x/front"));
        assert!(!ok("http://archive.org/download/x.jpg"));
        assert!(!ok("https://notarchive.org/x.jpg"));
        assert!(!ok("https://archive.org.example.com/x.jpg"));
        assert!(!ok("https://example.com/archive.org"));
    }
}
