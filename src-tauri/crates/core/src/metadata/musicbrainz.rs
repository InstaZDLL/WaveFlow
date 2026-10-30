//! MusicBrainz web service client, for the library enrichment engine.
//!
//! MusicBrainz is the one catalogue here with stable identifiers for
//! every level of a record — the recording, the release (one pressing),
//! the release group (the album, across its pressings) and the artist —
//! which is what lets the engine pair a local file with something it can
//! name again later, and the tag workshop write those names into files.
//!
//! # The service's rules, enforced here rather than by the caller
//!
//! - **At most one request a second**, per IP. The pacing is a
//!   process-wide [`Pacer`] shared by every client [`MusicBrainzClient::new`]
//!   builds, so two callers running at once still add up to one request
//!   a second instead of two.
//! - **A User-Agent naming the application and a way to reach it**.
//!   Anonymous agents are throttled first. The caller passes its version;
//!   the contact is the project's site.
//! - **503 is "slow down", not "nothing there"**. It comes back when the
//!   global rate is exceeded, and so does 429; both are retried after
//!   `Retry-After` (or a doubling backoff), a few times, before the
//!   failure is reported as a failure — never as an empty answer, which
//!   the engine would remember as "no match".
//!
//! Only reads, no key, nothing in the query that is not already in the
//! user's tags: the URL is still stripped from every error
//! ([`super::redact`]) because the query is the user's library.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{default_http, read_bounded, LookupError};

const BASE_URL: &str = "https://musicbrainz.org/ws/2";
const CONTACT: &str = "https://waveflow.app";
const TIMEOUT_SECS: u64 = 20;

/// The gap kept between two requests. A little over the documented
/// second, so a clock that rounds does not land two requests in one.
pub const MIN_INTERVAL: Duration = Duration::from_millis(1100);

/// The largest answer read. A release looked up with its recordings and
/// their relationships weighs about 11 kB a track; a box set of a
/// couple of hundred tracks stays under this, and anything larger is
/// refused rather than read whole into memory.
pub const MAX_RELEASE_BYTES: usize = 8 * 1024 * 1024;

/// How many times a throttled request is tried before it is reported.
const MAX_ATTEMPTS: u32 = 4;
/// The longest `Retry-After` honoured. A server asking for more than
/// this is down, and the engine is better off moving on and asking
/// again on its next pass.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// What the release lookup asks for: the tracklist, the credits of the
/// release and of each recording, the release group with its genres and
/// first release date, and the relationships that name performers
/// (recording level) and writers (work level).
const RELEASE_INC: &str = "recordings+artist-credits+release-groups+genres+labels\
+recording-level-rels+artist-rels+work-rels+work-level-rels";

/// Spaces requests so the process as a whole stays under the service's
/// rate.
///
/// A slot is reserved under the lock and slept on outside it, so callers
/// queue in the order they asked without holding a lock across an
/// `.await`.
#[derive(Debug)]
pub struct Pacer {
    interval: Duration,
    next: Mutex<Option<Instant>>,
}

impl Pacer {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: Mutex::new(None),
        }
    }

    /// The instant the caller may send, with the slot after it reserved
    /// for whoever asks next.
    fn reserve(&self) -> Instant {
        let now = Instant::now();
        let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner());
        let slot = match *next {
            Some(at) if at > now => at,
            _ => now,
        };
        *next = Some(slot + self.interval);
        slot
    }

    /// Wait for this caller's slot.
    pub async fn wait(&self) {
        let slot = self.reserve();
        tokio::time::sleep_until(tokio::time::Instant::from_std(slot)).await;
    }

    /// Push every later slot back by `delay`: the service asked the whole
    /// client to slow down, not just the request that was refused.
    fn back_off(&self, delay: Duration) {
        let mut next = self.next.lock().unwrap_or_else(|p| p.into_inner());
        let earliest = Instant::now() + delay;
        if next.map_or(true, |at| at < earliest) {
            *next = Some(earliest);
        }
    }
}

/// The pacer every [`MusicBrainzClient::new`] client shares — and the
/// one to hand a Cover Art Archive client, so both count against one
/// budget.
pub fn shared_pacer() -> Arc<Pacer> {
    static PACER: OnceLock<Arc<Pacer>> = OnceLock::new();
    PACER
        .get_or_init(|| Arc::new(Pacer::new(MIN_INTERVAL)))
        .clone()
}

/// A MusicBrainz identifier, checked before it goes into a path.
///
/// Identifiers come from the service itself and from file tags, and a
/// tag can hold anything: a value that is not a UUID is not one we send.
pub fn is_mbid(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::parse_str(value).is_ok()
}

#[derive(Clone)]
pub struct MusicBrainzClient {
    http: reqwest::Client,
    base_url: String,
    pacer: Arc<Pacer>,
    retry_floor: Duration,
}

impl MusicBrainzClient {
    /// A client for the public service, identified as WaveFlow
    /// `app_version`, sharing the process-wide pacing.
    pub fn new(app_version: &str) -> Self {
        Self {
            http: default_http(&user_agent(app_version), TIMEOUT_SECS),
            base_url: BASE_URL.to_owned(),
            pacer: shared_pacer(),
            retry_floor: Duration::from_secs(2),
        }
    }

    /// A client over the caller's HTTP client and address, with a pacer
    /// of its own. `retry_floor` is the first backoff when a throttled
    /// answer names no `Retry-After`; it doubles on each attempt.
    pub fn with_http(
        http: reqwest::Client,
        base_url: impl Into<String>,
        pacer: Arc<Pacer>,
        retry_floor: Duration,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            pacer,
            retry_floor,
        }
    }

    /// Releases whose title and artist look like `title` / `artist`, in
    /// the service's relevance order.
    ///
    /// A phrase query first, which is what keeps "Abbey Road" from
    /// matching every release with "Road" in its title; when it finds
    /// nothing — a title with a typo, an artist credited differently —
    /// the same words as loose terms.
    pub async fn search_releases(
        &self,
        title: &str,
        artist: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ReleaseSummary>, LookupError> {
        let phrase = release_query(title, artist, true);
        let found = self.search_releases_raw(&phrase, limit).await?;
        if !found.is_empty() {
            return Ok(found);
        }
        let loose = release_query(title, artist, false);
        if loose == phrase {
            return Ok(found);
        }
        self.search_releases_raw(&loose, limit).await
    }

    async fn search_releases_raw(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<ReleaseSummary>, LookupError> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 25).to_string();
        let page: ReleaseSearch = self
            .get_json(
                "release",
                &[("query", query), ("limit", &limit), ("fmt", "json")],
                super::MAX_RESPONSE_BYTES,
            )
            .await?
            .unwrap_or_default();
        Ok(page.releases)
    }

    /// One release with its tracklist, credits and release group.
    /// `Ok(None)` when the service does not know it, or `mbid` is not an
    /// identifier at all.
    pub async fn get_release(&self, mbid: &str) -> Result<Option<Release>, LookupError> {
        if !is_mbid(mbid) {
            return Ok(None);
        }
        self.get_json(
            &format!("release/{mbid}"),
            &[("inc", RELEASE_INC), ("fmt", "json")],
            MAX_RELEASE_BYTES,
        )
        .await
    }

    /// Artists named like `name`, in the service's relevance order.
    pub async fn search_artists(
        &self,
        name: &str,
        limit: u32,
    ) -> Result<Vec<ArtistSummary>, LookupError> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(Vec::new());
        }
        let query = format!("artist:\"{}\"", escape_phrase(name));
        let limit = limit.clamp(1, 25).to_string();
        let page: ArtistSearch = self
            .get_json(
                "artist",
                &[("query", &query), ("limit", &limit), ("fmt", "json")],
                super::MAX_RESPONSE_BYTES,
            )
            .await?
            .unwrap_or_default();
        Ok(page.artists)
    }

    /// One artist with its genres.
    pub async fn get_artist(&self, mbid: &str) -> Result<Option<ArtistSummary>, LookupError> {
        if !is_mbid(mbid) {
            return Ok(None);
        }
        self.get_json(
            &format!("artist/{mbid}"),
            &[("inc", "genres"), ("fmt", "json")],
            super::MAX_RESPONSE_BYTES,
        )
        .await
    }

    /// GET `path` with `query`, paced and retried. `Ok(None)` on 404.
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
        limit: usize,
    ) -> Result<Option<T>, LookupError> {
        let url = format!("{}/{}", self.base_url, path);
        let mut backoff = self.retry_floor;
        for attempt in 1..=MAX_ATTEMPTS {
            self.pacer.wait().await;
            let response = self
                .http
                .get(&url)
                .header(reqwest::header::ACCEPT, "application/json")
                .query(query)
                .send()
                .await?;
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if is_throttled(status) {
                let wait = retry_after(&response).unwrap_or(backoff);
                if attempt == MAX_ATTEMPTS {
                    return Err(LookupError::Status {
                        status: status.as_u16(),
                    });
                }
                tracing::debug!(
                    status = status.as_u16(),
                    attempt,
                    wait_ms = wait.as_millis() as u64,
                    "musicbrainz asked to slow down"
                );
                self.pacer.back_off(wait);
                backoff = (backoff * 2).min(MAX_RETRY_AFTER);
                continue;
            }
            if !status.is_success() {
                return Err(LookupError::Status {
                    status: status.as_u16(),
                });
            }
            let body = read_bounded(response, limit).await?;
            return Ok(Some(serde_json::from_slice(&body)?));
        }
        unreachable!("the last attempt returns from inside the loop")
    }
}

/// `WaveFlow/<version> ( <contact> )`, the shape the service documents.
pub fn user_agent(app_version: &str) -> String {
    format!("WaveFlow/{app_version} ( {CONTACT} )")
}

/// 503 is the service's rate limiter; 429 the one in front of it; 502
/// and 504 the proxy losing a backend for a moment. All four are worth
/// another try, the rest are an answer.
fn is_throttled(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let seconds: u64 = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

/// A Lucene query for a release: the title, and the artist when known.
///
/// `phrase` quotes each value; otherwise its words are escaped one by one
/// and searched as terms of the field.
pub fn release_query(title: &str, artist: Option<&str>, phrase: bool) -> String {
    let title = title.trim();
    if title.is_empty() {
        return String::new();
    }
    let field = |name: &str, value: &str| {
        if phrase {
            format!("{name}:\"{}\"", escape_phrase(value))
        } else {
            format!("{name}:({})", escape_terms(value))
        }
    };
    let mut query = field("release", title);
    if let Some(artist) = artist.map(str::trim).filter(|a| !a.is_empty()) {
        query.push_str(" AND ");
        query.push_str(&field("artist", artist));
    }
    query
}

/// Inside a quoted phrase only the quote and the backslash are special.
fn escape_phrase(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Outside quotes Lucene reads its whole operator set; every one of them
/// is escaped, so a title like "AC/DC - Live!" stays words.
fn escape_terms(value: &str) -> String {
    const SPECIAL: &[char] = &[
        '+', '-', '&', '|', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\',
        '/',
    ];
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

// ── Response types ──────────────────────────────────────────────────
//
// Faithful to the JSON the service sends, every field optional or
// defaulted: the schema is large, entries are edited by hand, and a
// field missing from one release is not a reason to drop the others.

#[derive(Debug, Default, Deserialize)]
struct ReleaseSearch {
    #[serde(default)]
    releases: Vec<ReleaseSummary>,
}

#[derive(Debug, Default, Deserialize)]
struct ArtistSearch {
    #[serde(default)]
    artists: Vec<ArtistSummary>,
}

/// A release as the search returns it: no tracklist.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleaseSummary {
    pub id: String,
    /// The search's own relevance, 0–100.
    #[serde(default)]
    pub score: Option<i64>,
    pub title: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub track_count: Option<i64>,
    #[serde(default)]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    pub release_group: Option<ReleaseGroup>,
}

/// A release with its tracklist and relationships.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Release {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    pub release_group: Option<ReleaseGroup>,
    #[serde(default)]
    pub genres: Vec<Genre>,
    #[serde(default)]
    pub media: Vec<Medium>,
    #[serde(default)]
    pub cover_art_archive: Option<CoverArtArchiveInfo>,
}

impl Release {
    pub fn track_count(&self) -> usize {
        self.media.iter().map(|m| m.tracks.len()).sum()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ReleaseGroup {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub primary_type: Option<String>,
    #[serde(default)]
    pub secondary_types: Vec<String>,
    #[serde(default)]
    pub first_release_date: Option<String>,
    #[serde(default)]
    pub genres: Vec<Genre>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Genre {
    pub name: String,
    /// Votes. A genre one editor added and one with fifty are not the
    /// same evidence.
    #[serde(default)]
    pub count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CoverArtArchiveInfo {
    #[serde(default)]
    pub front: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Medium {
    /// 1-based disc number.
    #[serde(default)]
    pub position: Option<i64>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Track {
    pub id: String,
    /// 1-based position on its medium.
    #[serde(default)]
    pub position: Option<i64>,
    #[serde(default)]
    pub title: String,
    /// Milliseconds.
    #[serde(default)]
    pub length: Option<i64>,
    #[serde(default)]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    pub recording: Option<Recording>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Recording {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub length: Option<i64>,
    #[serde(default)]
    pub first_release_date: Option<String>,
    #[serde(default)]
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Relation {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub target_type: String,
    #[serde(default)]
    pub attributes: Vec<String>,
    /// The name the release credits, when it differs from the artist's.
    #[serde(default)]
    pub target_credit: Option<String>,
    #[serde(default)]
    pub artist: Option<ArtistRef>,
    #[serde(default)]
    pub work: Option<Work>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Work {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub relations: Vec<Relation>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ArtistCredit {
    /// The name as credited on this release.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub joinphrase: Option<String>,
    pub artist: ArtistRef,
}

/// The credit as printed: names joined by their join phrases.
pub fn credit_text(credits: &[ArtistCredit]) -> String {
    let mut out = String::new();
    for credit in credits {
        let name = if credit.name.is_empty() {
            &credit.artist.name
        } else {
            &credit.name
        };
        out.push_str(name);
        if let Some(join) = &credit.joinphrase {
            out.push_str(join);
        }
    }
    out.trim().to_owned()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ArtistRef {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub disambiguation: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ArtistSummary {
    pub id: String,
    #[serde(default)]
    pub score: Option<i64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub disambiguation: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub area: Option<Area>,
    #[serde(default)]
    pub life_span: Option<LifeSpan>,
    #[serde(default)]
    pub genres: Vec<Genre>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Area {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LifeSpan {
    #[serde(default)]
    pub begin: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
}

#[cfg(test)]
mod tests;
