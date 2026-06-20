use serde_json::Value;

use crate::http::{safe_json, safe_text, validate_outbound_url};
use crate::{utils, Candidate, Result};

/// Hosts the Genius search response is permitted to redirect us to —
/// the search API can return URLs that point at `genius.com` proper or
/// the locale subdomain pattern that ultimately serves the lyric page.
const GENIUS_ALLOWED_HOSTS: &[&str] = &["genius.com", "www.genius.com"];

pub async fn search(
    http: &reqwest::Client,
    query: &str,
    cookie: Option<&str>,
) -> Result<Option<Candidate>> {
    let mut req = http
        .get("https://genius.com/api/search/multi")
        .query(&[("per_page", "5"), ("q", query)]);
    if let Some(cookie) = cookie {
        req = req.header(reqwest::header::COOKIE, cookie);
    }
    let response: Value =
        safe_json(req.send().await?.error_for_status()?).await?;
    // The multi-search response groups results into sections (song, lyric,
    // artist, album…) and the order is not contractual, so we can't index a
    // fixed slot. Prefer the dedicated "song" section, then fall back to the
    // first section that actually carries hits.
    let Some(url_str) = response["response"]["sections"]
        .as_array()
        .and_then(|sections| {
            sections
                .iter()
                .find(|section| section["type"].as_str() == Some("song"))
                .or_else(|| {
                    sections.iter().find(|section| {
                        section["hits"]
                            .as_array()
                            .is_some_and(|hits| !hits.is_empty())
                    })
                })
                .and_then(|section| section["hits"].as_array())
                .and_then(|hits| hits.first())
                .and_then(|hit| hit["result"]["url"].as_str())
        })
    else {
        return Ok(None);
    };
    // The lyric-page URL came out of a third-party JSON response — pin
    // it to genius.com before we follow it. Without this guard a
    // compromised Genius API response could ship a URL pointing at an
    // attacker-controlled host and we'd happily GET it.
    let url = validate_outbound_url(url_str, GENIUS_ALLOWED_HOSTS)?;
    let page = safe_text(http.get(url).send().await?.error_for_status()?).await?;
    let raw = extract_lyrics_containers(&page);
    let text = strip_genius_header(&raw);
    // Treat a stub page (header only, no lyrics body — Genius renders
    // these for songs that have been added but never transcribed) as a
    // miss so the host can fall through to the next provider instead of
    // surfacing the bare "N ContributorsTitle Lyrics" artifact to the
    // user (issue #284). The full unsynced lyrics are otherwise
    // returned unchanged.
    Ok((!text.trim().is_empty()).then_some(Candidate {
        synced: None,
        unsynced: Some(text),
    }))
}

fn extract_lyrics_containers(html: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while let Some(attr) = html[pos..].find("data-lyrics-container=\"true\"") {
        let attr = pos + attr;
        let Some(open_end) = html[attr..].find('>').map(|i| attr + i) else {
            break;
        };
        let Some(close) = html[open_end..].find("</div>").map(|i| open_end + i) else {
            break;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(utils::html_text_decode(&html[open_end + 1..close]).trim());
        pos = close + "</div>".len();
    }
    out
}

/// Strip the standard Genius lyrics-page header artifact from the
/// start of an extracted lyrics body.
///
/// Recent Genius layouts nest the contributor badge + the
/// `<song title> Lyrics` heading inside the same `data-lyrics-container`
/// div that holds the actual verses, so once `html_text_decode` flattens
/// the tags we end up with a prefix like
/// `"17 ContributorsFall on Me Lyrics"` glued directly to the lyrics body
/// (issue #284). Strip the prefix when we recognise the
/// `^\d+ Contributors.*? Lyrics` shape; leave the text untouched
/// otherwise so a different layout doesn't silently lose its first line.
fn strip_genius_header(text: &str) -> String {
    let trimmed = text.trim_start();
    let digits_end = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits_end == 0 {
        return text.to_string();
    }
    let after_digits = &trimmed[digits_end..];
    let Some(after_contrib) = after_digits.strip_prefix(" Contributors") else {
        return text.to_string();
    };
    // Find the first " Lyrics" token that closes the header — Genius
    // glues the title onto the trailing " Lyrics" word with a single
    // space separator (e.g. "...Vez Lyrics"). Anything before that is
    // the title we want to drop along with the badge text.
    let Some(lyrics_idx) = after_contrib.find(" Lyrics") else {
        return text.to_string();
    };
    after_contrib[lyrics_idx + " Lyrics".len()..]
        .trim_start()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_header_drops_badge_title_and_lyrics_token() {
        let input = "17 ContributorsFall on Me Lyrics\n[Chorus]\nFall on me";
        assert_eq!(
            strip_genius_header(input),
            "[Chorus]\nFall on me".to_string()
        );
    }

    #[test]
    fn strip_header_returns_empty_for_stub_pages() {
        // Genius renders this layout for songs added to the catalogue
        // without transcribed lyrics. The bare header is the only
        // content the parser sees, and the caller treats empty output
        // as a miss so the next provider gets a turn.
        let input = "2 ContributorsSolamente Una Vez Lyrics";
        assert_eq!(strip_genius_header(input), "".to_string());
    }

    #[test]
    fn strip_header_preserves_leading_whitespace_outside_pattern() {
        // Genius occasionally pads the container with leading
        // whitespace from inline scripts that don't decode to text.
        // The trim_start on the digit probe must NOT eat newlines that
        // belong to the lyrics body when no header is present.
        let input = "\n[Verse 1]\nA real lyric";
        assert_eq!(strip_genius_header(input), input.to_string());
    }

    #[test]
    fn strip_header_no_op_when_no_digit_prefix() {
        let input = "[Verse 1]\nA real lyric without a Genius header";
        assert_eq!(strip_genius_header(input), input.to_string());
    }

    #[test]
    fn strip_header_no_op_when_contributors_token_missing() {
        // A track titled with a leading number (e.g. "99 Red Balloons")
        // must not trigger the strip — only " Contributors" right
        // after the digits qualifies as a Genius header.
        let input = "99 Red Balloons Lyrics\n[Verse 1]";
        assert_eq!(strip_genius_header(input), input.to_string());
    }

    #[test]
    fn strip_header_no_op_when_lyrics_token_missing() {
        // Defensive: a Genius layout change that drops the trailing
        // " Lyrics" word would otherwise let us swallow the entire
        // body up to the first match. Leave the input alone instead
        // so the user sees raw content and we notice the regression
        // in bug reports rather than silent data loss.
        let input = "5 ContributorsSong Title Without Trailing Word\n[Verse]";
        assert_eq!(strip_genius_header(input), input.to_string());
    }

    #[test]
    fn strip_header_handles_multibyte_title() {
        // Solamente Una Vez carries no multibyte chars but other Latin
        // titles do (Naïve, Déjà Vu). The strip walks bytes via
        // `find(" Lyrics")` + `len()`, both byte-safe.
        let input = "3 ContributorsDéjà Vu Lyrics\n[Verse]\nMidnight again";
        assert_eq!(
            strip_genius_header(input),
            "[Verse]\nMidnight again".to_string()
        );
    }
}
