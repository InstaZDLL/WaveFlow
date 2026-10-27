//! TheAudioDB API client — multi-language artist biographies + wide
//! artist fanart.
//!
//! TheAudioDB is a community-maintained music database with a free v1
//! JSON API. We use it as an opt-in alternative to Last.fm for artist
//! bios (issue #295): unlike Last.fm it ships the biography in ~15
//! languages, so users who don't run a Last.fm account can still get a
//! localized bio.
//!
//! The same `search.php` response also carries the **wide** artist
//! images (`strArtistFanart*` / `strArtistWideThumb` /
//! `strArtistBanner`) that back the Spotify-style artist hero (issue
//! #482) — the rest of the pipeline only ever carried the square
//! Deezer photo. One lookup therefore serves both, which is why
//! [`TheAudioDbClient::artist_info`] returns bio *and* fanart instead
//! of a bio-only payload.
//!
//! `search.php?s=<name>` returns an `artists` array. The English bio is
//! the suffixless `strBiography`; other languages are `strBiography{XX}`
//! (e.g. `strBiographyFR`). We pick the requested language and fall back
//! to English when that language is empty.
//!
//! The free key (`123`) is shared and rate-limited to 30 req/min — fine
//! because the caller caches every result with a TTL, so a single user
//! stays far under the cap.

use serde::Deserialize;

use crate::metadata::name_match::{normalize_name, select_by_name};
use crate::metadata::{
    default_http, read_bounded, require_secure, BaseUrlError, LookupError, MAX_RESPONSE_BYTES,
};

const BASE_URL: &str = "https://www.theaudiodb.com/api/v1/json";
const FREE_API_KEY: &str = "123";
const USER_AGENT: &str = "WaveFlow/0.1";
const TIMEOUT_SECS: u64 = 6;

#[derive(Debug, Deserialize)]
struct SearchResponse {
    /// `null` when nothing matched the search term.
    artists: Option<Vec<ArtistPayload>>,
}

/// Only the fields we use. TheAudioDB returns every value as a JSON
/// string or null, so `Option<String>` is the honest type throughout.
///
/// `Default` is derived so tests can build a payload from the one or
/// two fields they exercise instead of spelling out every language.
#[derive(Debug, Default, Deserialize)]
struct ArtistPayload {
    #[serde(rename = "strArtist")]
    name: Option<String>,
    #[serde(rename = "strBiography")]
    bio_en: Option<String>,
    #[serde(rename = "strBiographyFR")]
    bio_fr: Option<String>,
    #[serde(rename = "strBiographyDE")]
    bio_de: Option<String>,
    #[serde(rename = "strBiographyES")]
    bio_es: Option<String>,
    #[serde(rename = "strBiographyIT")]
    bio_it: Option<String>,
    #[serde(rename = "strBiographyPT")]
    bio_pt: Option<String>,
    #[serde(rename = "strBiographyNL")]
    bio_nl: Option<String>,
    #[serde(rename = "strBiographyRU")]
    bio_ru: Option<String>,
    #[serde(rename = "strBiographyJP")]
    bio_jp: Option<String>,
    #[serde(rename = "strBiographyCN")]
    bio_cn: Option<String>,
    /// Wide 16:9 backdrops (1920×1080-ish). `strArtistFanart` is the
    /// primary one; 2/3/4 are alternates uploaded by the community.
    #[serde(rename = "strArtistFanart")]
    fanart: Option<String>,
    #[serde(rename = "strArtistFanart2")]
    fanart2: Option<String>,
    #[serde(rename = "strArtistFanart3")]
    fanart3: Option<String>,
    #[serde(rename = "strArtistFanart4")]
    fanart4: Option<String>,
    // `strArtistWideThumb` and `strArtistBanner` are deliberately not
    // deserialized: both are 1000×185 strips, and the hero they used to
    // feed is routinely wider than that — see `fanart_urls` (#693).
}

impl ArtistPayload {
    /// Pick the biography for `lang`, falling back to English when the
    /// requested language is missing or blank. `lang` is a short code
    /// (`"fr"`, `"de"`, … `"zh"`); anything unmapped resolves to English.
    fn bio_for_lang(&self, lang: &str) -> Option<String> {
        let primary = match lang {
            "fr" => &self.bio_fr,
            "de" => &self.bio_de,
            "es" => &self.bio_es,
            "it" => &self.bio_it,
            "pt" => &self.bio_pt,
            "nl" => &self.bio_nl,
            "ru" => &self.bio_ru,
            "ja" => &self.bio_jp,
            "zh" => &self.bio_cn,
            _ => &self.bio_en,
        };
        non_blank(primary).or_else(|| non_blank(&self.bio_en))
    }

    /// Every real fanart the artist has, in TheAudioDB's own order.
    ///
    /// **The wide thumb and the banner are deliberately absent** (issue
    /// #693). Both are 1000×185 strips by TheAudioDB's own spec, and
    /// the hero they were feeding is routinely wider than 1000 px and
    /// far taller than 185 px relative to its width — so they arrived
    /// upscaled and cropped to a sliver, which is what "the backdrop is
    /// low quality" looked like. With no fanart the hero falls back to
    /// the blurred square photo, which is honest about being an ambient
    /// colour field rather than a picture.
    fn fanart_urls(&self) -> Vec<String> {
        [&self.fanart, &self.fanart2, &self.fanart3, &self.fanart4]
            .into_iter()
            .filter_map(non_blank)
            .collect()
    }
}

/// Cleaned artist payload returned to callers. `bio_short` is a
/// truncated lead-in for the collapsed UI; `bio_full` is the whole
/// text; `fanart_urls` are the wide hero images (issue #482).
///
/// Every field is optional independently: an artist row can carry
/// fanart with no biography in any language, and vice-versa.
///
/// **All** the fanarts come back, not just the first one (issue #693):
/// an artist can have up to four, they differ enormously in framing,
/// and the picker lets the user choose. `fanart_url()` keeps the "just
/// give me one" caller honest.
#[derive(Debug, Clone)]
pub struct TheAudioDbArtist {
    pub name: String,
    pub bio_short: Option<String>,
    pub bio_full: Option<String>,
    pub fanart_urls: Vec<String>,
}

impl TheAudioDbArtist {
    /// The default hero image: the first fanart, which is TheAudioDB's
    /// own preferred one.
    pub fn fanart_url(&self) -> Option<&str> {
        self.fanart_urls.first().map(String::as_str)
    }
}

pub struct TheAudioDbClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Default for TheAudioDbClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TheAudioDbClient {
    /// The desktop's client, on the shared free key — see the module
    /// header for why one user stays under its cap.
    pub fn new() -> Self {
        Self {
            http: default_http(USER_AGENT, TIMEOUT_SECS),
            base_url: BASE_URL.to_owned(),
            api_key: FREE_API_KEY.to_owned(),
        }
    }

    /// A client over `http`, aimed at `base_url` (the root that ends in
    /// `/json`), asking under `api_key`. The free key is a test key the
    /// whole internet shares: anything serving more than one person
    /// should bring its own.
    ///
    /// Refused unless `base_url` is `https` or this machine: the key
    /// travels in the path.
    pub fn with_http(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, BaseUrlError> {
        Ok(Self {
            http,
            base_url: require_secure(base_url.into())?,
            api_key: api_key.into(),
        })
    }

    /// Look up an artist by name, returning its bio in `lang` (English
    /// fallback) and its wide fanart URL. Returns `Ok(None)` only when
    /// nothing matches the name — a match with neither bio nor fanart
    /// still comes back as `Some` with both fields empty, so the caller
    /// can cache the "looked it up, nothing there" outcome instead of
    /// re-querying a rate-limited API on every visit. A failure status
    /// is an `Err`: TheAudioDB has no in-band error to read instead.
    pub async fn artist_info(
        &self,
        name: &str,
        lang: &str,
    ) -> Result<Option<TheAudioDbArtist>, LookupError> {
        let url = format!("{}/{}/search.php", self.base_url, self.api_key);
        let response = self.http.get(url).query(&[("s", name)]).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(LookupError::Status {
                status: status.as_u16(),
            });
        }
        let resp: SearchResponse =
            serde_json::from_slice(&read_bounded(response, MAX_RESPONSE_BYTES).await?)?;

        let searched = normalize_name(name);
        let Some(artist) = resp
            .artists
            .and_then(|artists| select_by_name(artists, &searched, |a| a.name.as_deref()))
        else {
            return Ok(None);
        };

        let fanart_urls = artist.fanart_urls();
        let full = artist
            .bio_for_lang(lang)
            .map(clean_text)
            .filter(|full| !full.is_empty());

        Ok(Some(TheAudioDbArtist {
            name: artist.name.unwrap_or_default(),
            bio_short: full.as_deref().map(make_summary),
            bio_full: full,
            fanart_urls,
        }))
    }
}

fn non_blank(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Normalise line endings and trim. TheAudioDB bios are plain text
/// (no HTML), so paragraph breaks are preserved for the full view.
fn clean_text(input: String) -> String {
    input.replace("\r\n", "\n").trim().to_string()
}

/// Maximum length of the collapsed summary before truncation.
const SUMMARY_MAX: usize = 280;

/// Derive a short lead-in from the full bio: stop at the first blank
/// line (paragraph break) when that's already short enough, otherwise
/// truncate at a word boundary near `SUMMARY_MAX` and append an ellipsis.
///
/// `pub` so `commands::deezer::enrich_artist_deezer` (issue #343) can
/// reuse it to synthesize a `bio_short` for a manually-edited
/// `custom_bio` override — without it, the override set `bio_short ==
/// bio_full` verbatim and the frontend's "Read more" toggle (which
/// triggers on `bio_full.length > bio_short.length`) never appeared.
pub fn make_summary(full: &str) -> String {
    let first_para = full.split("\n\n").next().unwrap_or(full).trim();
    let collapsed = first_para.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SUMMARY_MAX {
        return collapsed;
    }
    let mut out = String::new();
    for word in collapsed.split(' ') {
        if out.chars().count() + word.chars().count() + 1 > SUMMARY_MAX {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests;
