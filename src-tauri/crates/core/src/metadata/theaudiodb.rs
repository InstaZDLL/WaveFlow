//! TheAudioDB API client — multi-language artist biographies.
//!
//! TheAudioDB is a community-maintained music database with a free v1
//! JSON API. We use it as an opt-in alternative to Last.fm for artist
//! bios (issue #295): unlike Last.fm it ships the biography in ~15
//! languages, so users who don't run a Last.fm account can still get a
//! localized bio.
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
#[derive(Debug, Deserialize)]
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
}

/// Cleaned artist bio returned to callers. `bio_short` is a truncated
/// lead-in for the collapsed UI; `bio_full` is the whole text.
#[derive(Debug, Clone)]
pub struct TheAudioDbArtistBio {
    pub name: String,
    pub bio_short: Option<String>,
    pub bio_full: Option<String>,
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
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    /// Look up an artist bio by name in `lang`. Returns `Ok(None)` when
    /// nothing matches or the matched artist has no biography in the
    /// requested language nor English.
    pub async fn artist_bio(
        &self,
        name: &str,
        lang: &str,
    ) -> reqwest::Result<Option<TheAudioDbArtistBio>> {
        let url = format!("{BASE_URL}/{FREE_API_KEY}/search.php");
        let resp: SearchResponse = self
            .http
            .get(url)
            .query(&[("s", name)])
            .send()
            .await?
            .json()
            .await?;

        let Some(artist) = resp.artists.and_then(|mut a| {
            if a.is_empty() {
                None
            } else {
                Some(a.swap_remove(0))
            }
        }) else {
            return Ok(None);
        };

        // Guard against homonyms / fuzzy hits: only trust the result when
        // its name matches what we searched for (case-insensitive) — the
        // same name-match the Deezer enrichment path applies before
        // accepting a search hit.
        if artist.name.as_deref().map(str::to_lowercase) != Some(name.to_lowercase()) {
            return Ok(None);
        }

        let Some(full) = artist.bio_for_lang(lang).map(clean_text) else {
            return Ok(None);
        };
        if full.is_empty() {
            return Ok(None);
        }

        Ok(Some(TheAudioDbArtistBio {
            name: artist.name.unwrap_or_default(),
            bio_short: Some(make_summary(&full)),
            bio_full: Some(full),
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
            bio_fr: None,
            bio_de: Some("  ".into()), // blank → ignored
            bio_es: None,
            bio_it: None,
            bio_pt: None,
            bio_nl: None,
            bio_ru: None,
            bio_jp: None,
            bio_cn: None,
        };
        assert_eq!(payload.bio_for_lang("fr").as_deref(), Some("English bio"));
        assert_eq!(payload.bio_for_lang("de").as_deref(), Some("English bio"));
    }
}
