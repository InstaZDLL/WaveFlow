use crate::{utils, Candidate, Result};

pub async fn search(http: &reqwest::Client, query: &str) -> Result<Option<Candidate>> {
    let page = http
        .get("https://www.megalobiz.com/search/all")
        .query(&[
            ("qry", query),
            ("searchButton.x", "0"),
            ("searchButton.y", "0"),
        ])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let Some(href) = best_lrc_href(&page, query) else {
        return Ok(None);
    };
    let detail = http
        .get(format!("https://www.megalobiz.com{href}"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let Some(id) = href.rsplit('.').next() else {
        return Ok(None);
    };
    let Some(raw) = extract_details_div(&detail, id) else {
        return Ok(None);
    };
    Ok(Some(classify(raw)))
}

fn best_lrc_href(html: &str, query: &str) -> Option<String> {
    let mut best = None;
    let mut best_score = -1.0;
    let mut pos = 0;
    while let Some(idx) = html[pos..].find("href=\"/lrc/maker/") {
        let href_start = pos + idx + "href=\"".len();
        let href_end = html[href_start..].find('"').map(|i| href_start + i)?;
        let tag_end = html[href_end..].find('>').map(|i| href_end + i)?;
        let close = html[tag_end..].find("</a>").map(|i| tag_end + i)?;
        let text = utils::html_text_decode(&html[tag_end + 1..close]);
        let label = comparable_text(&text, query);
        let score = utils::str_score(&label, query);
        if score > best_score {
            best_score = score;
            best = Some(html[href_start..href_end].to_string());
        }
        pos = close + "</a>".len();
    }
    (best_score >= 65.0).then_some(best).flatten()
}

fn comparable_text(text: &str, query: &str) -> String {
    let max_words = query.split_whitespace().count();
    text.split_whitespace()
        .filter(|tok| *tok != "by")
        .take(max_words)
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_details_div(html: &str, id: &str) -> Option<String> {
    let marker = format!("id=\"lrc_{id}_details\"");
    let attr = html.find(&marker)?;
    let open_end = html[attr..].find('>').map(|i| attr + i)?;
    let close = html[open_end..].find("</div>").map(|i| open_end + i)?;
    Some(
        utils::html_text_decode(&html[open_end + 1..close])
            .trim()
            .to_string(),
    )
}

fn classify(text: String) -> Candidate {
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
    }
}
