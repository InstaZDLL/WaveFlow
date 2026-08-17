//! Fetching a remote track's lyrics from the server (RFC-005).
//!
//! The server (`GET /api/v2/tracks/{id}/lyrics`, added in server PR #103)
//! returns its `LyricsList`: one or more structured lyric tracks, each a
//! list of `{ start?: ms, value }` lines with a `synced` flag. We pick the
//! best one (synced first) and flatten it into the LRC / plain text the
//! rest of WaveFlow's lyrics pipeline already speaks. A miss here (no
//! lyrics, 404, offline, not signed in) is not an error — the caller falls
//! back to LRCLIB by name.

use serde::Deserialize;

use crate::{error::AppResult, remote::client::RemoteClient, state::AppState};

#[derive(Deserialize)]
struct LyricsLineDto {
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StructuredLyricsDto {
    #[serde(default)]
    synced: bool,
    #[serde(default, rename = "line")]
    lines: Vec<LyricsLineDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LyricsListDto {
    #[serde(default)]
    structured_lyrics: Vec<StructuredLyricsDto>,
}

/// Server-sourced lyrics flattened to a single content blob plus whether
/// it carries line timestamps (so the caller stamps the payload's format).
pub struct ServerLyrics {
    pub content: String,
    pub synced: bool,
}

/// Format a millisecond offset as an `[mm:ss.xx]` LRC line tag — the same
/// centisecond shape the frontend's `parseLrc` expects.
fn lrc_stamp(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "[{:02}:{:02}.{:02}]",
        ms / 60_000,
        (ms % 60_000) / 1000,
        (ms % 1000) / 10,
    )
}

/// Fetch the server's lyrics for a remote track. `Ok(None)` for every
/// ordinary "no server lyrics" case (404, empty, offline, signed out, a
/// transient error) so the caller can fall back to LRCLIB — only a genuine
/// pool error propagates.
pub async fn fetch_server_lyrics(
    state: &AppState,
    track_id: &str,
) -> AppResult<Option<ServerLyrics>> {
    if crate::offline::is_offline() {
        return Ok(None);
    }
    // Signed out, or a token read / pre-emptive refresh failed — none of
    // these should abort the fetch: they just mean "no server lyrics", so we
    // fall through to the LRCLIB fallback rather than propagating. Lyrics are
    // best-effort, so even a local read error degrades to the fallback here.
    let client = match RemoteClient::try_build(state).await {
        Ok(Some(client)) => client,
        Ok(None) | Err(_) => return Ok(None),
    };

    let resp: LyricsListDto = match client
        .send_json(client.get(&format!("/api/v2/tracks/{track_id}/lyrics")))
        .await
    {
        Ok(list) => list,
        // A track with no lyrics answers 404; anything else here is a
        // transient hiccup. Either way we just fall back to LRCLIB.
        Err(failure) => {
            tracing::debug!(%failure, track = %track_id, "no server lyrics");
            return Ok(None);
        }
    };

    // Prefer a synced track with real content, else any track with content.
    let has_content =
        |s: &&StructuredLyricsDto| s.lines.iter().any(|l| !l.value.trim().is_empty());
    let best = resp
        .structured_lyrics
        .iter()
        .find(|s| s.synced && has_content(s))
        .or_else(|| resp.structured_lyrics.iter().find(has_content));
    let Some(best) = best else {
        return Ok(None);
    };

    let synced = best.synced;
    let mut content = String::new();
    for line in &best.lines {
        if synced {
            if let Some(start) = line.start {
                content.push_str(&lrc_stamp(start));
            }
        }
        content.push_str(&line.value);
        content.push('\n');
    }
    let content = content.trim_end().to_string();
    if content.is_empty() {
        return Ok(None);
    }
    Ok(Some(ServerLyrics { content, synced }))
}
