use serde_json::Value;

use crate::{utils, Candidate, Error, Result, SearchOptions};

const ROOT: &str = "https://apic-desktop.musixmatch.com/ws/1.1";

pub async fn search(http: &reqwest::Client, options: &SearchOptions) -> Result<Option<Candidate>> {
    let token = get_token(http).await?;
    let response: Value = http
        .get(format!("{ROOT}/track.search"))
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
        .error_for_status()?
        .json()
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
    let response: Value = http
        .get(format!("{ROOT}/token.get"))
        .query(&[
            ("user_language", "en"),
            ("app_id", "web-desktop-app-v1.0"),
            ("t", &timestamp_ms()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
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
    let response: Value = http
        .get(format!("{ROOT}/track.subtitle.get"))
        .query(&[
            ("track_id", track_id),
            ("subtitle_format", "lrc"),
            ("app_id", "web-desktop-app-v1.0"),
            ("usertoken", token),
            ("t", &timestamp_ms()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
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
    let response: Value = http
        .get(format!("{ROOT}/crowd.track.translations.get"))
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
        .error_for_status()?
        .json()
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
    let response: Value = http
        .get(format!("{ROOT}/track.richsync.get"))
        .query(&[
            ("track_id", track_id),
            ("app_id", "web-desktop-app-v1.0"),
            ("usertoken", token),
            ("t", &timestamp_ms()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
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
    let mut out = String::new();
    for row in rows {
        let ts = row["ts"].as_f64().unwrap_or_default();
        out.push_str(&format!("[{}] ", utils::format_time(ts)));
        if let Some(letters) = row["l"].as_array() {
            for letter in letters {
                let offset = letter["o"].as_f64().unwrap_or_default();
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
