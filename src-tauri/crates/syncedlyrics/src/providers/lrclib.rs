use serde::Deserialize;

use crate::http::safe_json;
use crate::{utils, Candidate, Result};

/// Minimum fuzzy score for a search hit to be considered the same track.
/// Mirrors the threshold used by the Musixmatch and Megalobiz providers so
/// the query-based fallback never latches onto an off-topic result.
const MIN_SCORE: f64 = 65.0;

#[derive(Debug, Deserialize)]
struct SearchTrack {
    id: serde_json::Value,
    #[serde(rename = "trackName")]
    track_name: String,
    #[serde(rename = "artistName")]
    artist_name: String,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetTrack {
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
}

pub async fn search(http: &reqwest::Client, query: &str) -> Result<Option<Candidate>> {
    let tracks: Vec<SearchTrack> = safe_json(
        http.get("https://lrclib.net/api/search")
            .query(&[("q", query)])
            .send()
            .await?
            .error_for_status()?,
    )
    .await?;
    let Some(best) = best_track(&tracks, query) else {
        return Ok(None);
    };
    // `best_track` already filtered out any candidate without an
    // extractable id (see the `extract_id` gate there) — an unknown
    // variant here would be a logic break, not a normal API miss,
    // so `expect` it instead of swallowing the search.
    let id = extract_id(&best.id).expect("best_track must yield an extractable id");
    let track: GetTrack = safe_json(
        http.get(format!("https://lrclib.net/api/get/{id}"))
            .send()
            .await?
            .error_for_status()?,
    )
    .await?;
    Ok(Some(Candidate {
        synced: non_empty(track.synced_lyrics),
        unsynced: non_empty(track.plain_lyrics),
    }))
}

/// Pull the LRCLIB row id out of the JSON `Value`. LRCLIB's contract
/// is that `id` is a number, but the field is typed `Value` here
/// because the search-list deserialiser doesn't know that up front;
/// `extract_id` returns `None` for any other shape so a poisoned entry
/// can be filtered out instead of aborting the whole search.
fn extract_id(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn best_track<'a>(tracks: &'a [SearchTrack], query: &str) -> Option<&'a SearchTrack> {
    let mut best = None;
    let mut best_score = -1.0;
    let mut best_cluster_count = 0usize;
    let mut best_has_synced = false;

    for track in tracks {
        // Skip any entry whose `id` shape isn't usable downstream —
        // without this guard the calling `search` function would
        // abort the whole query when the top-scoring hit happens to
        // carry a malformed id, even though other candidates in the
        // result set still had usable ones.
        if extract_id(&track.id).is_none() {
            continue;
        }
        let label = format!("{} - {}", track.artist_name, track.track_name);
        let score = utils::str_score(&label, query);
        let has_synced = track
            .synced_lyrics
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let cluster_count = count_similar_artist_matches(tracks, track, query);
        if is_better(
            score,
            has_synced,
            cluster_count,
            best_score,
            best_has_synced,
            best_cluster_count,
        ) {
            best = Some(track);
            best_score = score;
            best_cluster_count = cluster_count;
            best_has_synced = has_synced;
        }
    }
    if best_score < MIN_SCORE {
        return None;
    }
    best
}

fn is_better(
    score: f64,
    has_synced: bool,
    cluster_count: usize,
    best_score: f64,
    best_has_synced: bool,
    best_cluster_count: usize,
) -> bool {
    if score > best_score + 0.001 {
        return true;
    }
    if (score - best_score).abs() > 0.001 {
        return false;
    }
    if has_synced != best_has_synced {
        return has_synced;
    }
    cluster_count > best_cluster_count
}

fn count_similar_artist_matches(tracks: &[SearchTrack], track: &SearchTrack, query: &str) -> usize {
    tracks
        .iter()
        .filter(|other| {
            other
                .synced_lyrics
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        })
        .filter(|other| other.artist_name.eq_ignore_ascii_case(&track.artist_name))
        .filter(|other| utils::str_score(&other.track_name, &track.track_name) >= 90.0)
        .filter(|other| utils::str_score(&other.track_name, query) >= 90.0)
        .count()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}
