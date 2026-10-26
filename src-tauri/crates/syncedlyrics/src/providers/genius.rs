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
    let response: Value = safe_json(req.send().await?.error_for_status()?).await?;
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
    // Genius writes "1 Contributor" for a single transcriber and
    // "N Contributors" for two or more — the plural-s is conditional
    // on the badge count. Strip the singular core first; consume an
    // optional trailing 's' to cover both shapes without two near-
    // identical branches.
    let Some(after_contrib) = after_digits.strip_prefix(" Contributor") else {
        return text.to_string();
    };
    let after_contrib = after_contrib.strip_prefix('s').unwrap_or(after_contrib);
    // Find the " Lyrics" token that closes the header. Genius glues
    // the title onto a trailing " Lyrics" word with a single space
    // separator (e.g. "...Vez Lyrics") and immediately follows that
    // closer with either `\n` (when there's a body) or end-of-string
    // (when the catalogue entry has no transcribed lyrics yet).
    //
    // Neither `find` nor `rfind` is correct here in general: a song
    // titled "No Lyrics Needed" would put a " Lyrics" inside the
    // title (`find` cuts too early), and a body line like "Some
    // Lyrics" would do the same for `rfind` (cuts past the closer
    // into the verses). The header closer is the FIRST " Lyrics"
    // immediately followed by a newline or end-of-string, so we
    // iterate forward and stop at that specific shape.
    let lyrics_token = " Lyrics";
    let lyrics_idx = after_contrib
        .match_indices(lyrics_token)
        .find(|(i, _)| {
            let tail = &after_contrib[i + lyrics_token.len()..];
            tail.is_empty() || tail.starts_with('\n') || tail.starts_with('\r')
        })
        .map(|(i, _)| i);
    let Some(lyrics_idx) = lyrics_idx else {
        return text.to_string();
    };
    after_contrib[lyrics_idx + lyrics_token.len()..]
        .trim_start()
        .to_string()
}

#[cfg(test)]
mod tests;
