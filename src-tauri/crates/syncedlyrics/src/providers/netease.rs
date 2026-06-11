use serde_json::Value;

use crate::http::safe_json;
use crate::{utils, Candidate, Result};

pub async fn search(
    http: &reqwest::Client,
    query: &str,
    cookie: Option<&str>,
) -> Result<Option<Candidate>> {
    let mut req = http.get("https://music.163.com/api/search/pc").query(&[
        ("limit", "10"),
        ("type", "1"),
        ("offset", "0"),
        ("s", query),
    ]);
    if let Some(cookie) = cookie {
        req = req.header(reqwest::header::COOKIE, cookie);
    }
    let response: Value = safe_json(req.send().await?.error_for_status()?).await?;
    let Some(songs) = response["result"]["songs"].as_array() else {
        return Ok(None);
    };
    let Some(id) = best_song_id(songs, query) else {
        return Ok(None);
    };
    lyrics_by_id(http, id, cookie).await
}

fn best_song_id(songs: &[Value], query: &str) -> Option<i64> {
    songs
        .iter()
        .filter_map(|song| {
            let name = song["name"].as_str().unwrap_or_default();
            let artist = song["artists"]
                .as_array()
                .and_then(|artists| artists.first())
                .and_then(|artist| artist["name"].as_str())
                .unwrap_or_default();
            let score = utils::str_score(&format!("{name} {artist}"), query);
            let id = song["id"].as_i64()?;
            Some((score, id))
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .and_then(|(score, id)| (score >= 65.0).then_some(id))
}

async fn lyrics_by_id(
    http: &reqwest::Client,
    id: i64,
    cookie: Option<&str>,
) -> Result<Option<Candidate>> {
    let mut req = http
        .get("https://music.163.com/api/song/lyric")
        .query(&[("id", id.to_string()), ("lv", "1".to_string())]);
    if let Some(cookie) = cookie {
        req = req.header(reqwest::header::COOKIE, cookie);
    }
    let response: Value = safe_json(req.send().await?.error_for_status()?).await?;
    let Some(lyric) = response["lrc"]["lyric"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(None);
    };
    let text = lyric.to_string();
    Ok(Some(
        if matches!(
            utils::detect_format(&text),
            crate::LyricsFormat::Lrc | crate::LyricsFormat::EnhancedLrc
        ) {
            Candidate {
                synced: Some(text),
                unsynced: None,
            }
        } else {
            Candidate {
                synced: None,
                unsynced: Some(text),
            }
        },
    ))
}
