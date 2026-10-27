use reqwest::Response;
use serde_json::Value;

use crate::http::{redact_url, safe_json};
use crate::{utils, Candidate, Error, Result, SearchOptions};

const ROOT: &str = "https://apic-desktop.musixmatch.com/ws/1.1";

/// Send a Musixmatch GET request and wrap any failure with a redacted
/// URL + `reqwest::Error::without_url()`.
///
/// The Musixmatch API takes `usertoken` as a query parameter. Without
/// this wrapper, a network reset / 5xx / TLS error from `.send()` or
/// `.error_for_status()` would propagate up via `Error::Http(reqwest::
/// Error)` whose `Display` includes the full request URL — token and
/// all — into `tracing::warn!(?err, …)` lines in [`commands/lyrics.
/// rs`](crate::commands::lyrics) and into the IPC error returned to
/// the frontend. The token is short-lived but a defence-in-depth gap
/// is still a gap.
///
/// The helper:
///
/// 1. Materialises the URL (with query) once via
///    [`reqwest::RequestBuilder::build`] so it can run
///    [`redact_url`] against the final string.
/// 2. Calls [`reqwest::Error::without_url`] on any failure so the
///    inner error's `Display` drops the URL field entirely.
/// 3. Re-attaches the REDACTED URL as context in the new
///    [`Error::Transport`] message so the operator still gets the
///    `host + path` for diagnostics, just without the credential —
///    and the host still learns the provider could not be reached.
/// 4. Keeps the stripped error as that variant's `source`, so a caller
///    that walks the chain still reads why (refused, DNS, timeout).
async fn redacted_get(
    http: &reqwest::Client,
    url: String,
    query: &[(&str, &str)],
    context: &'static str,
) -> Result<Response> {
    let request = http.get(&url).query(query);
    // `try_clone()` returns `None` for streaming bodies — none of
    // the Musixmatch endpoints stream, so the fallback path only
    // triggers if the request builder itself rejects, in which
    // case the pre-query URL is still safer than nothing.
    let redacted = match request.try_clone().and_then(|r| r.build().ok()) {
        Some(built) => redact_url(built.url().as_str()),
        None => redact_url(&url),
    };

    let response = request.send().await.map_err(|err| Error::Transport {
        message: format!("{context} request failed at {redacted}"),
        source: Some(err.without_url()),
    })?;

    response.error_for_status().map_err(|err| Error::Transport {
        message: format!("{context} returned non-2xx at {redacted}"),
        source: Some(err.without_url()),
    })
}

pub async fn search(http: &reqwest::Client, options: &SearchOptions) -> Result<Option<Candidate>> {
    let token = get_token(http).await?;
    let response: Value = safe_json(
        redacted_get(
            http,
            format!("{ROOT}/track.search"),
            &[
                ("q", options.query.as_str()),
                ("page_size", "5"),
                ("page", "1"),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token.as_str()),
                ("t", &timestamp_ms()),
            ],
            "musixmatch search",
        )
        .await?,
    )
    .await?;

    if response["message"]["header"]["status_code"].as_i64() != Some(200) {
        return Ok(None);
    }
    let Some(tracks) = response["message"]["body"]["track_list"].as_array() else {
        return Ok(None);
    };
    let Some(track_id) = best_track_id(tracks, &options.query) else {
        return Ok(None);
    };
    let track_id = track_id.to_string();

    if options.enhanced {
        if let Some(candidate) = richsync(http, &token, &track_id).await? {
            if candidate.synced.is_some() {
                return Ok(Some(candidate));
            }
        }
    }

    subtitle(http, &token, &track_id, options.lang.as_deref()).await
}

async fn get_token(http: &reqwest::Client) -> Result<String> {
    // `token.get` doesn't ride a token in its query (it's the request
    // that MINTS one), so the URL doesn't carry a credential. Still
    // routed through `redacted_get` for uniform error-shape across
    // every Musixmatch endpoint — the helper is idempotent on
    // already-clean URLs.
    let response: Value = safe_json(
        redacted_get(
            http,
            format!("{ROOT}/token.get"),
            &[
                ("user_language", "en"),
                ("app_id", "web-desktop-app-v1.0"),
                ("t", &timestamp_ms()),
            ],
            "musixmatch token",
        )
        .await?,
    )
    .await?;
    response["message"]["body"]["user_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Provider("Musixmatch token missing".into()))
}

fn best_track_id(tracks: &[Value], query: &str) -> Option<i64> {
    tracks
        .iter()
        .filter_map(|item| {
            let track = item.get("track")?;
            let name = track.get("track_name")?.as_str().unwrap_or_default();
            let artist = track.get("artist_name")?.as_str().unwrap_or_default();
            let label = format!("{name} {artist}");
            let score = utils::str_score(&label, query);
            let id = track.get("track_id")?.as_i64()?;
            Some((score, id))
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .and_then(|(score, id)| (score >= 65.0).then_some(id))
}

async fn subtitle(
    http: &reqwest::Client,
    token: &str,
    track_id: &str,
    lang: Option<&str>,
) -> Result<Option<Candidate>> {
    let response: Value = safe_json(
        redacted_get(
            http,
            format!("{ROOT}/track.subtitle.get"),
            &[
                ("track_id", track_id),
                ("subtitle_format", "lrc"),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token),
                ("t", &timestamp_ms()),
            ],
            "musixmatch subtitle",
        )
        .await?,
    )
    .await?;
    let Some(mut lrc) = response["message"]["body"]["subtitle"]["subtitle_body"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
    else {
        return Ok(None);
    };

    if let Some(lang) = lang {
        // Translation enrichment is best-effort: a network blip /
        // rate-limit / decoded-but-empty response must not poison
        // the underlying synced lyrics — fall through to the raw
        // LRC and log instead of bubbling the error up. Closing
        // the dead-code branch flagged on PR #207 (issue #208).
        match add_translations(http, token, track_id, lang, &lrc).await {
            Ok(with_translations) => lrc = with_translations,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    lang,
                    "musixmatch translation enrichment failed; serving untranslated LRC",
                );
            }
        }
    }
    Ok(Some(Candidate {
        synced: Some(lrc),
        unsynced: None,
    }))
}

/// Strip the leading `[mm:ss.xx]` / `[mm:ss]` / `[mm:ss.xxx]` LRC
/// timestamp from a line and return `(timestamps, text)`. Multiple
/// stamp brackets per line (Musixmatch + foobar2000 repeat a chorus
/// line under several timestamps) are all peeled off, preserved in
/// original order so the caller can re-emit the translation under
/// the same stamps.
///
/// Returns an empty `Vec` for the stamps when the line carries
/// none (header lines `[ar:…]` / `[ti:…]` / blank LRC headers also
/// match `[…]` but their tag isn't a numeric stamp — we detect
/// numeric by checking the first byte is ASCII digit).
fn split_lrc_line(line: &str) -> (Vec<&str>, &str) {
    let mut stamps = Vec::new();
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        let Some(rest_after_open) = trimmed.strip_prefix('[') else {
            break;
        };
        let Some(close_idx) = rest_after_open.find(']') else {
            break;
        };
        let inside = &rest_after_open[..close_idx];
        // A numeric LRC stamp starts with an ASCII digit. Header
        // tags like `[ar:Artist]` start with a letter and we
        // deliberately leave them alone — they end up in the
        // `text` half so the line round-trips unchanged.
        if !inside
            .bytes()
            .next()
            .map(|b| b.is_ascii_digit())
            .unwrap_or(false)
        {
            break;
        }
        // Find the byte index of `]` relative to the *trimmed* slice,
        // then map it back to the original `rest` so the stamp slice
        // we push points into the original string. Layout of `rest`:
        //   `[leading_ws]['['][close_idx bytes of stamp content][']']`
        // → stamp_end = leading_ws + 1 (the `[`) + close_idx + 1 (the `]`).
        let leading_ws = rest.len() - trimmed.len();
        let stamp_end = leading_ws + 1 + close_idx + 1;
        stamps.push(&rest[leading_ws..stamp_end]);
        rest = &rest[stamp_end..];
    }
    (stamps, rest)
}

async fn add_translations(
    http: &reqwest::Client,
    token: &str,
    track_id: &str,
    lang: &str,
    lrc: &str,
) -> Result<String> {
    let response: Value = safe_json(
        redacted_get(
            http,
            format!("{ROOT}/crowd.track.translations.get"),
            &[
                ("track_id", track_id),
                ("subtitle_format", "lrc"),
                ("translation_fields_set", "minimal"),
                ("selected_language", lang),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token),
                ("t", &timestamp_ms()),
            ],
            "musixmatch translations",
        )
        .await?,
    )
    .await?;
    let Some(translations) = response["message"]["body"]["translations_list"].as_array() else {
        return Ok(lrc.to_string());
    };
    // Pre-index `(original_line_text → translation)` so the per-line
    // walk below is O(1) per row. The Musixmatch payload returns
    // `subtitle_matched_line` as the bare lyric text (no LRC stamp
    // prefix), which is why the previous `lrc.replace(original, …)`
    // never hit — every LRC line carries `[mm:ss.xx]` ahead of the
    // text. We strip the stamp before matching and re-emit the
    // translation under the same stamp(s).
    let mut table: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in translations {
        let Some(original) = item["translation"]["subtitle_matched_line"].as_str() else {
            continue;
        };
        let Some(translated) = item["translation"]["description"].as_str() else {
            continue;
        };
        let original = original.trim();
        let translated = translated.trim();
        if original.is_empty() || translated.is_empty() {
            continue;
        }
        table.insert(original.to_string(), translated.to_string());
    }
    if table.is_empty() {
        return Ok(lrc.to_string());
    }

    let mut out = String::with_capacity(lrc.len() * 2);
    for raw_line in lrc.split_inclusive('\n') {
        // Preserve trailing CR/LF on the source line so the rebuilt
        // LRC keeps the original line ending.
        let line_terminator: &str = raw_line
            .find(['\r', '\n'])
            .map(|idx| &raw_line[idx..])
            .unwrap_or("");
        let line_no_eol = raw_line.trim_end_matches(['\r', '\n']);
        out.push_str(line_no_eol);
        out.push_str(line_terminator);
        let (stamps, text) = split_lrc_line(line_no_eol);
        if stamps.is_empty() {
            continue;
        }
        let lookup = text.trim();
        if lookup.is_empty() {
            continue;
        }
        let Some(translated) = table.get(lookup) else {
            continue;
        };
        // Emit a follow-up line wearing the same stamp prefix so the
        // player groups the translation with its original cue (LRC
        // readers stable-sort by stamp). Parens flag the translation
        // visually for plain-text consumers.
        for stamp in &stamps {
            out.push_str(stamp);
        }
        out.push('(');
        out.push_str(translated);
        out.push(')');
        out.push_str(line_terminator);
    }
    Ok(out)
}

/// Validate that the richsync rows form a real word-level lyric.
///
/// Returns `Some(stamps)` when every row carries a finite, non-negative
/// numeric `ts` AND there are at least 4 rows AND at least 80 % of the
/// stamps are unique. `None` means the row set must be rejected.
///
/// Used by both the live `richsync` path and the unit tests; the
/// caller in `commands/lyrics/fetch.rs::fetch_lyrics` caches a Musixmatch
/// hit with `EnhancedLrc` format AHEAD of LRCLIB, so a corrupted
/// row set must not slip through — Musixmatch occasionally returns
/// rows where `ts` is missing or shaped as a string, which the
/// previous `as_f64().unwrap_or_default()` path silently coerced to
/// `0.0` (producing a fake word-level LRC with every line stamped
/// `[00:00.00]`).
fn validate_rows(rows: &[Value]) -> Option<Vec<f64>> {
    let mut stamps: Vec<f64> = Vec::with_capacity(rows.len());
    for row in rows {
        let ts = row.get("ts").and_then(Value::as_f64)?;
        if !ts.is_finite() || ts < 0.0 {
            return None;
        }
        stamps.push(ts);
    }
    if stamps.len() < 4 {
        return None;
    }
    let unique_ratio = {
        let mut sorted = stamps.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted.dedup();
        sorted.len() as f64 / stamps.len() as f64
    };
    if unique_ratio < 0.80 {
        return None;
    }
    Some(stamps)
}

async fn richsync(
    http: &reqwest::Client,
    token: &str,
    track_id: &str,
) -> Result<Option<Candidate>> {
    let response: Value = safe_json(
        redacted_get(
            http,
            format!("{ROOT}/track.richsync.get"),
            &[
                ("track_id", track_id),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token),
                ("t", &timestamp_ms()),
            ],
            "musixmatch richsync",
        )
        .await?,
    )
    .await?;
    if response["message"]["header"]["status_code"].as_i64() != Some(200) {
        return Ok(None);
    }
    let Some(raw) = response["message"]["body"]["richsync"]["richsync_body"].as_str() else {
        return Ok(None);
    };
    let rows: Value = serde_json::from_str(raw)?;
    let Some(rows) = rows.as_array() else {
        return Ok(None);
    };
    // Reject up-front via the shared validator so tests + production
    // share a single source of truth for what counts as real
    // word-level. See [`validate_rows`] for the contract.
    if validate_rows(rows).is_none() {
        return Ok(None);
    }
    let mut out = String::new();
    for row in rows {
        // Safe to expect: `validate_rows` already proved every row
        // has a finite non-negative `ts`.
        let ts = row.get("ts").and_then(Value::as_f64).expect("validated");
        out.push_str(&richsync_line(ts, row["l"].as_array().map(Vec::as_slice)));
        out.push('\n');
    }
    Ok((!out.trim().is_empty()).then_some(Candidate {
        synced: Some(out),
        unsynced: None,
    }))
}

/// One richsync row as an Enhanced LRC line: the row's stamp, then each
/// segment of `l` behind its own stamp, written exactly as Musixmatch
/// cuts it. The spaces between words are segments of their own, so a
/// segment is never padded: padding read as the document's own spacing,
/// and a line cut per character (Chinese, Japanese) was drawn one
/// character per word.
fn richsync_line(ts: f64, segments: Option<&[Value]>) -> String {
    let mut line = format!("[{}]", utils::format_time(ts));
    for segment in segments.unwrap_or_default() {
        let offset = segment
            .get("o")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite() && *f >= 0.0)
            .unwrap_or(0.0);
        let c = segment["c"].as_str().unwrap_or_default();
        line.push_str(&format!("<{}>{}", utils::format_time(ts + offset), c));
    }
    line
}

fn timestamp_ms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

#[cfg(test)]
mod tests;
