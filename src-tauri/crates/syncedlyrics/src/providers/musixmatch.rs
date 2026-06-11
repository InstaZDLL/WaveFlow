use serde_json::Value;

use crate::http::safe_json;
use crate::{utils, Candidate, Error, Result, SearchOptions};

const ROOT: &str = "https://apic-desktop.musixmatch.com/ws/1.1";

pub async fn search(http: &reqwest::Client, options: &SearchOptions) -> Result<Option<Candidate>> {
    let token = get_token(http).await?;
    let response: Value = safe_json(
        http.get(format!("{ROOT}/track.search"))
            .query(&[
                ("q", options.query.as_str()),
                ("page_size", "5"),
                ("page", "1"),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token.as_str()),
                ("t", &timestamp_ms()),
            ])
            .send()
            .await?
            .error_for_status()?,
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
    let response: Value = safe_json(
        http.get(format!("{ROOT}/token.get"))
            .query(&[
                ("user_language", "en"),
                ("app_id", "web-desktop-app-v1.0"),
                ("t", &timestamp_ms()),
            ])
            .send()
            .await?
            .error_for_status()?,
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
        http.get(format!("{ROOT}/track.subtitle.get"))
            .query(&[
                ("track_id", track_id),
                ("subtitle_format", "lrc"),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token),
                ("t", &timestamp_ms()),
            ])
            .send()
            .await?
            .error_for_status()?,
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
        lrc = add_translations(http, token, track_id, lang, lrc).await?;
    }
    Ok(Some(Candidate {
        synced: Some(lrc),
        unsynced: None,
    }))
}

async fn add_translations(
    http: &reqwest::Client,
    token: &str,
    track_id: &str,
    lang: &str,
    mut lrc: String,
) -> Result<String> {
    let response: Value = safe_json(
        http.get(format!("{ROOT}/crowd.track.translations.get"))
            .query(&[
                ("track_id", track_id),
                ("subtitle_format", "lrc"),
                ("translation_fields_set", "minimal"),
                ("selected_language", lang),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token),
                ("t", &timestamp_ms()),
            ])
            .send()
            .await?
            .error_for_status()?,
    )
    .await?;
    if let Some(translations) = response["message"]["body"]["translations_list"].as_array() {
        for item in translations {
            let original = item["translation"]["subtitle_matched_line"].as_str();
            let translated = item["translation"]["description"].as_str();
            if let (Some(original), Some(translated)) = (original, translated) {
                lrc = lrc.replace(original, &format!("{original}\n({translated})"));
            }
        }
    }
    Ok(lrc)
}

async fn richsync(
    http: &reqwest::Client,
    token: &str,
    track_id: &str,
) -> Result<Option<Candidate>> {
    let response: Value = safe_json(
        http.get(format!("{ROOT}/track.richsync.get"))
            .query(&[
                ("track_id", track_id),
                ("app_id", "web-desktop-app-v1.0"),
                ("usertoken", token),
                ("t", &timestamp_ms()),
            ])
            .send()
            .await?
            .error_for_status()?,
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
    // Validate every row carries a real numeric `ts` BEFORE building
    // the output. Musixmatch occasionally returns rows where `ts` is
    // missing or shaped as a string instead of a number; the previous
    // `as_f64().unwrap_or_default()` path silently coerced those to
    // `0.0`, producing a fake word-level LRC where every line was
    // stamped `[00:00.00]`. The caller (`commands/lyrics.rs`) caches
    // any Musixmatch result with EnhancedLrc format AHEAD of LRCLIB,
    // so a corrupted richsync would defeat the "real word-level only"
    // gate the PR description advertises.
    let mut out = String::new();
    let mut row_stamps: Vec<f64> = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(ts) = row.get("ts").and_then(Value::as_f64) else {
            return Ok(None);
        };
        if !ts.is_finite() || ts < 0.0 {
            return Ok(None);
        }
        row_stamps.push(ts);
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
    // Need at least 4 rows AND ≥ 80 % unique line stamps before we
    // call this a real word-level result. A track whose every line
    // shares the same `ts` is either an outright bug in the upstream
    // response OR a corrupted record (Musixmatch returns these for
    // some instrumental tracks); either way it must not poison the
    // cache as "word-level lyrics found".
    if row_stamps.len() < 4 {
        return Ok(None);
    }
    let unique_ratio = {
        let mut sorted = row_stamps.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted.dedup();
        sorted.len() as f64 / row_stamps.len() as f64
    };
    if unique_ratio < 0.80 {
        return Ok(None);
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
    //! These tests exercise the richsync validation in isolation: the
    //! function under test (`richsync`) is async + does HTTP, so we
    //! factor the validation logic into a sibling helper that takes
    //! the parsed rows and reproduce the regression #5 scenarios.
    //!
    //! Replication: copy the body of the row-validation loop and the
    //! ratio check into `validate_rows_for_test` below so the public
    //! `richsync` function stays a single network-boundary entry point.

    use serde_json::json;

    /// Strict copy of the validation logic in `richsync`. Returns
    /// `None` when the rows should be rejected (= the production
    /// function would return Ok(None)), `Some(stamps)` otherwise.
    fn validate_rows_for_test(rows: &[serde_json::Value]) -> Option<Vec<f64>> {
        let mut stamps: Vec<f64> = Vec::with_capacity(rows.len());
        for row in rows {
            let ts = row.get("ts").and_then(serde_json::Value::as_f64)?;
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

    #[test]
    fn rejects_all_zero_timestamps() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
        ];
        assert!(validate_rows_for_test(&rows).is_none());
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
        assert!(validate_rows_for_test(&rows).is_none());
    }

    #[test]
    fn rejects_too_few_rows() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
        ];
        assert!(validate_rows_for_test(&rows).is_none());
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
        assert!(validate_rows_for_test(&rows).is_none());
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
        assert!(validate_rows_for_test(&rows).is_some());
    }

    #[test]
    fn rejects_negative_or_non_finite_timestamps() {
        let rows = vec![
            json!({"ts": -1.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
        ];
        assert!(validate_rows_for_test(&rows).is_none());

        let rows = vec![
            json!({"ts": f64::NAN, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
        ];
        assert!(validate_rows_for_test(&rows).is_none());
    }
}
