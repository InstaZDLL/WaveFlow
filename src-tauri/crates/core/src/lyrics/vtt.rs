//! WebVTT, as a [`Document`]. Reader only: a transcript is never written.
//!
//! The header, comments and style blocks are skipped; a cue's settings are
//! ignored; a `<v Speaker>` voice tag names the line's agent, and every other
//! tag goes. A block that does not parse is skipped rather than refusing the
//! file, since a transcript with one bad cue is still a transcript.

use super::cue::{document_from, parse_timing, Cue, MAX_CUES, MAX_CUE_CHARS};
use super::model::{split_words, Document};
use super::ParseError;
use crate::markup;

/// Reads a WebVTT file. Refuses one that does not open with `WEBVTT`: an error
/// page served in its place must not become lines to follow.
pub fn parse(content: &str) -> Result<Document, ParseError> {
    let content = content.trim_start_matches('\u{feff}');
    let opens_right = content.starts_with("WEBVTT")
        && content[6..]
            .chars()
            .next()
            .map_or(true, |c| c.is_whitespace());
    if !opens_right {
        return Err(ParseError::NotTimedText);
    }
    let mut cues = Vec::new();
    let mut lines = content.lines();
    // The header block, up to the first blank line.
    lines.by_ref().find(|line| line.trim().is_empty());
    let mut block: Vec<&str> = Vec::new();
    for line in lines.chain(std::iter::once("")) {
        if !line.trim().is_empty() {
            block.push(line);
            continue;
        }
        if let Some(cue) = read_block(&block) {
            cues.push(cue);
            if cues.len() >= MAX_CUES {
                break;
            }
        }
        block.clear();
    }
    Ok(document_from(cues))
}

/// A block is a cue when its first or second line is a timing: the first of
/// the two may be the cue's identifier. `NOTE`, `STYLE` and `REGION` blocks
/// hold no timing and fall out here.
fn read_block(block: &[&str]) -> Option<Cue> {
    let timing_at = block.iter().take(2).position(|line| line.contains("-->"))?;
    let (begin_ms, end_ms) = parse_timing(block[timing_at])?;
    let payload = block[timing_at + 1..].join("\n");
    let speaker = voice_of(&payload);
    let text = markup::collapse(&markup::cue_to_text(&payload), MAX_CUE_CHARS);
    Some(Cue {
        begin_ms,
        end_ms,
        speaker,
        words: split_words(&text),
    })
}

/// The name in the first `<v Name>` or `<v.class Name>`.
fn voice_of(payload: &str) -> Option<String> {
    let start = payload.find("<v")?;
    let rest = &payload[start + 2..];
    if !rest.starts_with([' ', '\t', '.']) {
        return None;
    }
    let tag = &rest[..rest.find('>')?];
    let (_, name) = tag.split_once([' ', '\t'])?;
    let name = markup::decode_entities(name).trim().to_owned();
    (!name.is_empty()).then_some(name)
}
