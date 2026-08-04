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
    /// ~1000×185 wide thumbnail — narrower than fanart but still a
    /// usable hero strip when no fanart exists.
    #[serde(rename = "strArtistWideThumb")]
    wide_thumb: Option<String>,
    /// 1000×185 banner, usually carrying the artist's logo. Last
    /// resort: the text baked into it can clash with the header copy.
    #[serde(rename = "strArtistBanner")]
    banner: Option<String>,
}

impl ArtistPayload {
    /// Selects the biography for a supported language and falls back to English when the selected biography is unavailable or blank.
    ///
    /// Supported language codes are `fr`, `de`, `es`, `it`, `pt`, `nl`, `ru`, `ja`, and `zh`; other codes select English.
    ///
    /// # Examples
    ///
    /// ```
    /// let payload = ArtistPayload {
    ///     bio_en: Some("English biography".into()),
    ///     ..Default::default()
    /// };
    ///
    /// assert_eq!(payload.bio_for_lang("fr"), Some("English biography".into()));
    /// ```
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

    /// Selects the first available wide artist image in priority order.
    ///
    /// Blank image URLs are skipped. Fanart fields take precedence over the wide thumbnail,
    /// which takes precedence over the banner.
    ///
    /// # Examples
    ///
    /// ```
    /// let payload = ArtistPayload {
    ///     fanart: Some("https://example.com/fanart.jpg".into()),
    ///     ..Default::default()
    /// };
    ///
    /// assert_eq!(
    ///     payload.fanart_url(),
    ///     Some("https://example.com/fanart.jpg".into())
    /// );
    /// ```
    fn fanart_url(&self) -> Option<String> {
        non_blank(&self.fanart)
            .or_else(|| non_blank(&self.fanart2))
            .or_else(|| non_blank(&self.fanart3))
            .or_else(|| non_blank(&self.fanart4))
            .or_else(|| non_blank(&self.wide_thumb))
            .or_else(|| non_blank(&self.banner))
    }
}

/// Cleaned artist payload returned to callers. `bio_short` is a
/// truncated lead-in for the collapsed UI; `bio_full` is the whole
/// text; `fanart_url` is the wide hero image (issue #482).
///
/// Every field is optional independently: an artist row can carry
/// fanart with no biography in any language, and vice-versa.
#[derive(Debug, Clone)]
pub struct TheAudioDbArtist {
    pub name: String,
    pub bio_short: Option<String>,
    pub bio_full: Option<String>,
    pub fanart_url: Option<String>,
}

pub struct TheAudioDbClient {
    http: reqwest::Client,
}

impl Default for TheAudioDbClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TheAudioDbClient {
    /// Creates a client for communicating with TheAudioDB.
    ///
    /// # Examples
    ///
    /// ```
    /// let _client = TheAudioDbClient::new();
    /// ```
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// Looks up an artist by name and returns localized biography and fanart information.
    ///
    /// The biography uses the requested language with an English fallback. A matching
    /// artist is returned even when no biography or fanart is available; `None` means
    /// that no artist matched the requested name.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// async fn lookup_artist(
    ///     client: &TheAudioDbClient,
    /// ) -> reqwest::Result<()> {
    ///     let artist = client.artist_info("Daft Punk", "en").await?;
    ///
    ///     if let Some(artist) = artist {
    ///         println!("{}", artist.name);
    ///     }
    ///
    ///     Ok(())
    /// }
    /// ```
    pub async fn artist_info(
        &self,
        name: &str,
        lang: &str,
    ) -> reqwest::Result<Option<TheAudioDbArtist>> {
        let url = format!("{BASE_URL}/{FREE_API_KEY}/search.php");
        let resp: SearchResponse = self
            .http
            .get(url)
            .query(&[("s", name)])
            .send()
            .await?
            .json()
            .await?;

        let searched = normalize_name(name);
        let Some(artist) = resp
            .artists
            .and_then(|artists| select_by_name(artists, &searched, |a| a.name.as_deref()))
        else {
            return Ok(None);
        };

        let fanart_url = artist.fanart_url();
        let full = artist
            .bio_for_lang(lang)
            .map(clean_text)
            .filter(|full| !full.is_empty());

        Ok(Some(TheAudioDbArtist {
            name: artist.name.unwrap_or_default(),
            bio_short: full.as_deref().map(make_summary),
            bio_full: full,
            fanart_url,
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
mod tests {
    use super::*;

    #[test]
    fn summary_keeps_short_paragraph_intact() {
        let full = "Daft Punk are a French duo.\n\nSecond paragraph here.";
        assert_eq!(make_summary(full), "Daft Punk are a French duo.");
    }

    #[test]
    fn summary_truncates_long_text_at_word_boundary() {
        let full = "word ".repeat(100);
        let summary = make_summary(full.trim());
        assert!(summary.ends_with('…'));
        assert!(summary.chars().count() <= SUMMARY_MAX + 1);
        // Never cuts mid-word.
        assert!(!summary.trim_end_matches('…').ends_with("wor"));
    }

    #[test]
    fn bio_for_lang_falls_back_to_english() {
        let payload = ArtistPayload {
            name: Some("X".into()),
            bio_en: Some("English bio".into()),
            bio_de: Some("  ".into()), // blank → ignored
            ..Default::default()
        };
        assert_eq!(payload.bio_for_lang("fr").as_deref(), Some("English bio"));
        assert_eq!(payload.bio_for_lang("de").as_deref(), Some("English bio"));
    }

    #[test]
    fn fanart_url_prefers_the_widest_image() {
        let payload = ArtistPayload {
            fanart: Some("  ".into()), // blank → skipped
            fanart2: Some("https://cdn/fanart2.jpg".into()),
            wide_thumb: Some("https://cdn/wide.jpg".into()),
            banner: Some("https://cdn/banner.jpg".into()),
            ..Default::default()
        };
        assert_eq!(
            payload.fanart_url().as_deref(),
            Some("https://cdn/fanart2.jpg")
        );
    }

    #[test]
    fn fanart_url_falls_back_to_banner() {
        let payload = ArtistPayload {
            banner: Some("https://cdn/banner.jpg".into()),
            ..Default::default()
        };
        assert_eq!(
            payload.fanart_url().as_deref(),
            Some("https://cdn/banner.jpg")
        );
        assert_eq!(ArtistPayload::default().fanart_url(), None);
    }
}
