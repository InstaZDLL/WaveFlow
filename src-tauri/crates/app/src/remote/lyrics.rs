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

    Ok(structured_to_lyrics(&resp.structured_lyrics))
}

fn track_has_content(track: &StructuredLyricsDto) -> bool {
    track.lines.iter().any(|l| !l.value.trim().is_empty())
}

/// Every non-empty line carries a timestamp — the condition for rendering a
/// track as synced LRC without dropping lines in the frontend's `parseLrc`.
fn track_fully_stamped(track: &StructuredLyricsDto) -> bool {
    track
        .lines
        .iter()
        .all(|l| l.value.trim().is_empty() || l.start.is_some())
}

/// Pick the best structured track and flatten it into `ServerLyrics`. Split
/// out of the fetch so it's unit-testable without a network round-trip.
fn structured_to_lyrics(tracks: &[StructuredLyricsDto]) -> Option<ServerLyrics> {
    // Prefer a synced track that is ALSO complete (every non-empty line
    // timestamped) — a synced-but-partial one would be downgraded to plain
    // below, so it must not shadow a later fully-stamped track. Otherwise the
    // first track with any content, rendered plain if it isn't fully stamped.
    let best = tracks
        .iter()
        .find(|s| s.synced && track_has_content(s) && track_fully_stamped(s))
        .or_else(|| tracks.iter().find(|s| track_has_content(s)))?;

    let synced = best.synced && track_fully_stamped(best);
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
    // Drop only the single trailing newline the loop appended — keep any
    // trailing spaces or blank final lines the server sent verbatim.
    if content.ends_with('\n') {
        content.pop();
    }
    if content.is_empty() {
        return None;
    }
    Some(ServerLyrics { content, synced })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: Option<i64>, value: &str) -> LyricsLineDto {
        LyricsLineDto {
            start,
            value: value.to_string(),
        }
    }
    fn track(synced: bool, lines: Vec<LyricsLineDto>) -> StructuredLyricsDto {
        StructuredLyricsDto { synced, lines }
    }

    #[test]
    fn lrc_stamp_covers_boundaries() {
        assert_eq!(lrc_stamp(0), "[00:00.00]");
        assert_eq!(lrc_stamp(999), "[00:00.99]");
        assert_eq!(lrc_stamp(1000), "[00:01.00]");
        assert_eq!(lrc_stamp(61_230), "[01:01.23]");
        // Negatives are clamped to zero rather than producing a bogus stamp.
        assert_eq!(lrc_stamp(-5), "[00:00.00]");
    }

    #[test]
    fn fully_stamped_track_renders_synced_lrc() {
        let got = structured_to_lyrics(&[track(
            true,
            vec![line(Some(0), "one"), line(Some(1500), "two")],
        )])
        .unwrap();
        assert!(got.synced);
        assert_eq!(got.content, "[00:00.00]one\n[00:01.50]two");
    }

    #[test]
    fn partially_stamped_track_falls_back_to_plain_keeping_all_lines() {
        let got =
            structured_to_lyrics(&[track(true, vec![line(Some(0), "one"), line(None, "two")])])
                .unwrap();
        assert!(!got.synced);
        // Both lines survive, without any `[mm:ss.xx]` tag.
        assert_eq!(got.content, "one\ntwo");
    }

    #[test]
    fn a_complete_synced_track_wins_over_an_earlier_partial_one() {
        let got = structured_to_lyrics(&[
            track(true, vec![line(Some(0), "partial"), line(None, "gap")]),
            track(
                true,
                vec![line(Some(0), "full"), line(Some(1000), "stamped")],
            ),
        ])
        .unwrap();
        assert!(got.synced);
        assert_eq!(got.content, "[00:00.00]full\n[00:01.00]stamped");
    }

    #[test]
    fn no_usable_content_returns_none() {
        assert!(structured_to_lyrics(&[]).is_none());
        assert!(structured_to_lyrics(&[track(false, vec![line(None, "   ")])]).is_none());
    }

    #[test]
    fn trailing_spaces_and_blank_lines_survive() {
        let got = structured_to_lyrics(&[track(
            false,
            vec![line(None, "hello  "), line(None, ""), line(None, "")],
        )])
        .unwrap();
        // Only the loop's final '\n' is stripped: the trailing spaces on the
        // first line and the two blank lines are preserved.
        assert_eq!(got.content, "hello  \n\n");
    }
}
