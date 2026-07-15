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

        let Some(artist) =
            resp.artists.and_then(|artists| pick_artist(artists, &normalize_name(name)))
        else {
            return Ok(None);
        };

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

/// Pick the artist entry that best matches the normalized search term.
///
/// TheAudioDB's `search.php?s=<name>` returns a *relevance-ranked* array,
/// but the top hit is often a canonical superset ("Bob Marley & The
/// Wailers" for a library tagged "Bob Marley"). The old code took index 0
/// and demanded an exact case-insensitive name equality, so any such
/// superset — or a mere punctuation difference like "Boney M." vs
/// "Boney M" — silently dropped the bio (issue #342).
///
/// Now we scan the whole list: an exact normalized match wins wherever it
/// sits, and failing that we accept the first word-boundary prefix match
/// in either direction (subset ↔ superset). Normalization folds case,
/// common Latin diacritics, `&`/"and", punctuation and whitespace, so
/// homonym protection is preserved (unrelated names still don't match)
/// while trivial spelling variance no longer blocks a hit.
fn pick_artist(artists: Vec<ArtistPayload>, searched: &str) -> Option<ArtistPayload> {
    if searched.is_empty() {
        return None;
    }
    let mut prefix_match: Option<ArtistPayload> = None;
    for artist in artists {
        let Some(candidate) = artist.name.as_deref().map(normalize_name) else {
            continue;
        };
        if candidate == searched {
            return Some(artist);
        }
        if prefix_match.is_none() && names_prefix_compatible(searched, &candidate) {
            prefix_match = Some(artist);
        }
    }
    prefix_match
}

/// Normalise an artist name for fuzzy comparison: lowercase, fold common
/// Latin diacritics, expand `&` to "and", drop punctuation and collapse
/// whitespace to single spaces. "Boney M." and "Boney M" both become
/// "boney m"; "Bob Marley & The Wailers" becomes "bob marley and the
/// wailers"; "Beyoncé" becomes "beyonce". Non-Latin scripts (CJK, …) are
/// preserved verbatim minus punctuation.
fn normalize_name(name: &str) -> String {
    let lowered = name.to_lowercase().replace('&', " and ");
    let mut out = String::with_capacity(lowered.len());
    let mut pending_space = false;
    for ch in lowered.chars() {
        // Drop NFD combining marks (the accent half of a decomposed
        // character) so "Bjo\u{308}rk" folds to the same "bjork" as its
        // precomposed NFC form — without this they'd act as a boundary
        // and split the word. NOT a word boundary, so we `continue`
        // rather than fall into the punctuation branch below.
        if is_combining_mark(ch) {
            continue;
        }
        let folded = fold_diacritic(ch);
        if folded.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(folded);
        } else {
            // Punctuation or whitespace → a single word boundary.
            pending_space = true;
        }
    }
    out
}

/// True for Unicode nonspacing combining marks — the accent halves left
/// standing in an NFD-decomposed string. Covers the standard combining
/// blocks; dependency-free stand-in for the `Mn` general category so we
/// don't pull in `unicode-normalization` just to fold accents.
fn is_combining_mark(ch: char) -> bool {
    matches!(ch as u32,
        0x0300..=0x036F | // Combining Diacritical Marks
        0x1AB0..=0x1AFF | // Combining Diacritical Marks Extended
        0x1DC0..=0x1DFF | // Combining Diacritical Marks Supplement
        0x20D0..=0x20FF | // Combining Diacritical Marks for Symbols
        0xFE20..=0xFE2F,  // Combining Half Marks
    )
}

/// Map a lowercase accented Latin char to its base letter; pass anything
/// else through unchanged. Covers the accents common in Western artist
/// names without pulling in a Unicode-normalization dependency.
fn fold_diacritic(ch: char) -> char {
    match ch {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
        'ñ' | 'ń' => 'n',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => 'o',
        'ś' | 'š' => 's',
        'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// True when one normalized name is a word-boundary prefix of the other
/// (or they're equal). Both inputs are single-space-joined with no
/// leading/trailing space, so requiring the remainder to start with a
/// space keeps the match anchored to a whole word — "bob marley" matches
/// "bob marley and the wailers" but not "bob marleyx".
fn names_prefix_compatible(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if short.is_empty() {
        return false;
    }
    match long.strip_prefix(short) {
        Some(rest) => rest.is_empty() || rest.starts_with(' '),
        None => false,
    }
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

    /// Build a payload with only a name + an English bio derived from it,
    /// so tests can assert *which* entry `pick_artist` selected.
    fn named(name: &str) -> ArtistPayload {
        ArtistPayload {
            name: Some(name.into()),
            bio_en: Some(format!("bio of {name}")),
            bio_fr: None,
            bio_de: None,
            bio_es: None,
            bio_it: None,
            bio_pt: None,
            bio_nl: None,
            bio_ru: None,
            bio_jp: None,
            bio_cn: None,
        }
    }

    #[test]
    fn normalize_folds_punctuation_case_and_diacritics() {
        assert_eq!(normalize_name("Boney M."), "boney m");
        assert_eq!(normalize_name("Boney M"), "boney m");
        assert_eq!(
            normalize_name("Bob Marley & The Wailers"),
            "bob marley and the wailers"
        );
        assert_eq!(normalize_name("Beyoncé"), "beyonce");
        assert_eq!(normalize_name("Guns N' Roses"), "guns n roses");
        // Non-Latin script survives (minus punctuation).
        assert_eq!(normalize_name("宇多田ヒカル"), "宇多田ヒカル");
    }

    #[test]
    fn normalize_is_stable_across_nfc_and_nfd_forms() {
        // "Björk": precomposed ö (NFC) vs o + combining diaeresis (NFD).
        let bjork_nfc = "Bj\u{00F6}rk";
        let bjork_nfd = "Bjo\u{0308}rk";
        assert_eq!(normalize_name(bjork_nfc), "bjork");
        assert_eq!(normalize_name(bjork_nfd), "bjork");
        assert_eq!(normalize_name(bjork_nfc), normalize_name(bjork_nfd));

        // "Beyoncé": precomposed é (NFC) vs e + combining acute (NFD).
        let beyonce_nfc = "Beyonc\u{00E9}";
        let beyonce_nfd = "Beyonce\u{0301}";
        assert_eq!(normalize_name(beyonce_nfd), "beyonce");
        assert_eq!(normalize_name(beyonce_nfc), normalize_name(beyonce_nfd));
    }

    #[test]
    fn pick_exact_match_wins_regardless_of_position() {
        // The relevant entry sits after a homonym; exact match must still win.
        let artists = vec![named("Nirvana (60s band)"), named("Nirvana")];
        let picked = pick_artist(artists, &normalize_name("Nirvana")).unwrap();
        assert_eq!(picked.name.as_deref(), Some("Nirvana"));
    }

    #[test]
    fn pick_punctuation_only_difference_matches() {
        // "Boney M." (search) vs "Boney M" (DB) — issue #342.
        let artists = vec![named("Boney M")];
        let picked = pick_artist(artists, &normalize_name("Boney M.")).unwrap();
        assert_eq!(picked.name.as_deref(), Some("Boney M"));
    }

    #[test]
    fn pick_superset_name_matches_via_prefix() {
        // Library tagged "Bob Marley", TheAudioDB canonical is the band — #342.
        let artists = vec![named("Bob Marley & The Wailers")];
        let picked = pick_artist(artists, &normalize_name("Bob Marley")).unwrap();
        assert_eq!(picked.name.as_deref(), Some("Bob Marley & The Wailers"));
    }

    #[test]
    fn pick_prefers_exact_over_prefix() {
        let artists = vec![named("Bob Marley & The Wailers"), named("Bob Marley")];
        let picked = pick_artist(artists, &normalize_name("Bob Marley")).unwrap();
        assert_eq!(picked.name.as_deref(), Some("Bob Marley"));
    }

    #[test]
    fn pick_rejects_unrelated_homonym() {
        // "Bob Dylan" must not satisfy a "Bob Marley" search.
        let artists = vec![named("Bob Dylan")];
        assert!(pick_artist(artists, &normalize_name("Bob Marley")).is_none());
        // A shared first word alone is not a word-boundary prefix match.
        assert!(!names_prefix_compatible("bob marley", "bob dylan"));
    }

    #[test]
    fn pick_empty_search_is_none() {
        assert!(pick_artist(vec![named("Anything")], "").is_none());
    }
}
