use serde_json::Value;

use crate::{utils, Candidate, Result};

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
    let response: Value = req.send().await?.error_for_status()?.json().await?;
    let Some(url) = response["response"]["sections"]
        .as_array()
        .and_then(|sections| sections.get(1))
        .and_then(|section| section["hits"].as_array())
        .and_then(|hits| hits.first())
        .and_then(|hit| hit["result"]["url"].as_str())
    else {
        return Ok(None);
    };
    let page = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let text = extract_lyrics_containers(&page);
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
