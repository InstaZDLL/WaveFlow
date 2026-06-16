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
///    [`Error::Provider`] message so the operator still gets the
///    `host + path` for diagnostics, just without the credential.
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

    let response = request.send().await.map_err(|err| {
        Error::Provider(format!(
            "{context} request failed at {redacted}: {}",
            err.without_url(),
        ))
    })?;

    response.error_for_status().map_err(|err| {
        Error::Provider(format!(
            "{context} returned non-2xx at {redacted}: {}",
            err.without_url(),
        ))
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
/// caller in `commands/lyrics.rs::fetch_lyrics` caches a Musixmatch
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
        out.push_str(&format!("[{}] ", utils::format_time(ts)));
        if let Some(letters) = row["l"].as_array() {
            for letter in letters {
                let offset = letter
                    .get("o")
                    .and_then(Value::as_f64)
                    .filter(|f| f.is_finite() && *f >= 0.0)
                    .unwrap_or(0.0);
                let c = letter["c"].as_str().unwrap_or_default();
                out.push_str(&format!("<{}> {} ", utils::format_time(ts + offset), c));
            }
        }
        out.push('\n');
    }
    Ok((!out.trim().is_empty()).then_some(Candidate {
        synced: Some(out),
        unsynced: None,
    }))
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
mod richsync_tests {
    //! Tests for the regression #5 scenarios. Exercise the shared
    //! [`validate_rows`] helper directly so production + tests run
    //! against a single source of truth — when the production path
    //! evolves, the tests follow automatically.

    use super::validate_rows;
    use serde_json::json;

    #[test]
    fn rejects_all_zero_timestamps() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn rejects_string_timestamps() {
        // Musixmatch occasionally returns `ts` as a JSON string. The
        // pre-fix code coerced to `0.0`; the fix must reject.
        let rows = vec![
            json!({"ts": "1.0", "l": []}),
            json!({"ts": "2.0", "l": []}),
            json!({"ts": "3.0", "l": []}),
            json!({"ts": "4.0", "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn rejects_too_few_rows() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn rejects_majority_duplicate_timestamps() {
        // 5 rows, only 2 unique → 40 % unique, below the 80 % gate.
        let rows = vec![
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn accepts_well_formed_rows() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
            json!({"ts": 4.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_some());
    }

    #[test]
    fn rejects_negative_or_non_finite_timestamps() {
        let rows = vec![
            json!({"ts": -1.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());

        let rows = vec![
            json!({"ts": f64::NAN, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }
}

#[cfg(test)]
mod redact_tests {
    //! GH #234 — Musixmatch passes `usertoken` as a query parameter.
    //! Verify the redaction path: even when the underlying
    //! `reqwest::Error` would normally Display with the full URL,
    //! the error returned by [`redacted_get`] must not contain the
    //! literal token.
    //!
    //! Tests don't hit the real Musixmatch — they point `redacted_get`
    //! at an unresolvable hostname so `send().await` errors out
    //! deterministically with a DNS/connect failure. The exact error
    //! reason doesn't matter; the property we assert is "the token
    //! string never appears in the formatted error".

    use super::redacted_get;

    const SECRET: &str = "deadbeefcafe1234usertokenSECRET";

    fn client() -> reqwest::Client {
        // Short timeouts so the test fails fast on real networks too.
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(500))
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("client builds")
    }

    #[tokio::test]
    async fn send_failure_drops_token_from_displayed_error() {
        // `.invalid` is RFC 6761-reserved — guaranteed never to
        // resolve, so the DNS layer rejects deterministically.
        let url = "https://musixmatch-token-redact-test.invalid/track.search".to_string();
        let err = redacted_get(
            &client(),
            url,
            &[
                ("q", "any"),
                ("usertoken", SECRET),
                ("app_id", "web-desktop-app-v1.0"),
            ],
            "test endpoint",
        )
        .await
        .expect_err("DNS for .invalid must fail");
        let rendered = format!("{err}");
        assert!(
            !rendered.contains(SECRET),
            "token leaked into formatted error: {rendered}",
        );
        // Sanity: the redacted host + path should still be there for
        // operator diagnostics.
        assert!(
            rendered.contains("musixmatch-token-redact-test.invalid"),
            "redacted host context missing: {rendered}",
        );
        assert!(
            rendered.contains("/track.search"),
            "redacted path context missing: {rendered}",
        );
    }

    #[tokio::test]
    async fn debug_formatted_error_also_drops_token() {
        // Belt-and-suspenders: some `tracing::warn!(?err, …)` sites
        // use `Debug` rather than `Display`. The `without_url()`
        // strip must hold there too.
        let url = "https://musixmatch-token-redact-test.invalid/track.search".to_string();
        let err = redacted_get(
            &client(),
            url,
            &[("usertoken", SECRET)],
            "test endpoint",
        )
        .await
        .expect_err("DNS for .invalid must fail");
        let rendered = format!("{err:?}");
        assert!(
            !rendered.contains(SECRET),
            "token leaked into Debug-formatted error: {rendered}",
        );
        // Symmetry with `send_failure_drops_token_from_displayed_error`:
        // the diagnostic context (redacted host + path) must survive
        // through the Debug formatter so an operator chasing a
        // `tracing::warn!(?err, …)` line in the rolling log still
        // sees WHICH endpoint failed.
        assert!(
            rendered.contains("musixmatch-token-redact-test.invalid"),
            "redacted host context missing from Debug: {rendered}",
        );
        assert!(
            rendered.contains("/track.search"),
            "redacted path context missing from Debug: {rendered}",
        );
    }
}

#[cfg(test)]
mod split_lrc_line_tests {
    use super::split_lrc_line;

    #[test]
    fn peels_single_numeric_stamp() {
        let (stamps, text) = split_lrc_line("[00:12.34]Hello world");
        assert_eq!(stamps, vec!["[00:12.34]"]);
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn peels_multi_stamp_chorus_line() {
        let (stamps, text) = split_lrc_line("[00:30.10][01:05.40][02:11.00]Chorus line");
        assert_eq!(stamps, vec!["[00:30.10]", "[01:05.40]", "[02:11.00]"]);
        assert_eq!(text, "Chorus line");
    }

    #[test]
    fn preserves_header_tags() {
        // `[ar:Artist]` starts with a letter, not a digit — must stay
        // in the text half so the LRC header lines round-trip.
        let (stamps, text) = split_lrc_line("[ar:The Artist]");
        assert!(stamps.is_empty());
        assert_eq!(text, "[ar:The Artist]");
    }

    #[test]
    fn handles_empty_line() {
        let (stamps, text) = split_lrc_line("");
        assert!(stamps.is_empty());
        assert_eq!(text, "");
    }

    #[test]
    fn handles_stamp_only_line() {
        // Musixmatch occasionally returns instrumental marker lines
        // as bare stamps with empty text.
        let (stamps, text) = split_lrc_line("[01:30.00]");
        assert_eq!(stamps, vec!["[01:30.00]"]);
        assert_eq!(text, "");
    }

    #[test]
    fn accepts_seconds_only_stamp() {
        // LRC dialect with `[ss]` instead of `[mm:ss.xx]` is rare but
        // still starts with a digit — must be peeled too.
        let (stamps, text) = split_lrc_line("[00]intro");
        assert_eq!(stamps, vec!["[00]"]);
        assert_eq!(text, "intro");
    }
}

#[cfg(test)]
mod merge_translations_tests {
    use super::split_lrc_line;
    use std::collections::HashMap;

    // Pure-helper variant of the merge loop in `add_translations`,
    // factored here so the test doesn't have to roundtrip through
    // a real Musixmatch HTTP call. Mirrors the exact algorithm
    // (split → lookup trim → re-emit under same stamps).
    fn merge(lrc: &str, table: &HashMap<String, String>) -> String {
        let mut out = String::with_capacity(lrc.len() * 2);
        for raw_line in lrc.split_inclusive('\n') {
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
            for stamp in &stamps {
                out.push_str(stamp);
            }
            out.push('(');
            out.push_str(translated);
            out.push(')');
            out.push_str(line_terminator);
        }
        out
    }

    #[test]
    fn injects_translation_below_original_with_same_stamp() {
        let lrc = "[00:12.34]Hello world\n[00:15.00]How are you\n";
        let mut table = HashMap::new();
        table.insert("Hello world".into(), "Bonjour le monde".into());
        table.insert("How are you".into(), "Comment ça va".into());
        let out = merge(lrc, &table);
        assert_eq!(
            out,
            "[00:12.34]Hello world\n[00:12.34](Bonjour le monde)\n[00:15.00]How are you\n[00:15.00](Comment ça va)\n",
        );
    }

    #[test]
    fn skips_lines_without_a_match() {
        let lrc = "[00:12.34]Untranslated line\n";
        let table = HashMap::new();
        let out = merge(lrc, &table);
        assert_eq!(out, lrc);
    }

    #[test]
    fn preserves_lrc_header_tags() {
        let lrc = "[ti:Song Title]\n[ar:The Artist]\n[00:12.34]Hello world\n";
        let mut table = HashMap::new();
        table.insert("Hello world".into(), "Salut".into());
        let out = merge(lrc, &table);
        assert!(out.contains("[ti:Song Title]"));
        assert!(out.contains("[ar:The Artist]"));
        assert!(out.contains("[00:12.34](Salut)"));
    }

    #[test]
    fn handles_multi_stamp_chorus() {
        let lrc = "[00:30.10][01:05.40]Chorus\n";
        let mut table = HashMap::new();
        table.insert("Chorus".into(), "Refrain".into());
        let out = merge(lrc, &table);
        // Translation rides under the SAME compound stamp prefix so
        // an LRC reader that stable-sorts by stamp groups the pair.
        assert_eq!(
            out,
            "[00:30.10][01:05.40]Chorus\n[00:30.10][01:05.40](Refrain)\n",
        );
    }

    #[test]
    fn ignores_empty_text_lines() {
        // A bare-stamp instrumental marker has nothing to translate.
        let lrc = "[01:30.00]\n";
        let mut table = HashMap::new();
        table.insert("".into(), "should not appear".into());
        let out = merge(lrc, &table);
        assert_eq!(out, "[01:30.00]\n");
    }

    #[test]
    fn round_trips_crlf_line_endings() {
        let lrc = "[00:12.34]Hello\r\n[00:15.00]World\r\n";
        let mut table = HashMap::new();
        table.insert("Hello".into(), "Salut".into());
        let out = merge(lrc, &table);
        assert_eq!(
            out,
            "[00:12.34]Hello\r\n[00:12.34](Salut)\r\n[00:15.00]World\r\n",
        );
    }
}
