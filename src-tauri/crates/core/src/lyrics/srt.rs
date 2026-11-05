//! SubRip, as a [`Document`]. Reader only: a transcript is never written.
//!
//! A block is an optional counter, a timing, and text with the formatting
//! tags SRT files pick up along the way (`<i>`, `{\an8}`), which go. A block
//! that does not parse is skipped.

use super::cue::{document_from, parse_timing, Cue, MAX_CUES, MAX_CUE_CHARS};
use super::model::{split_words, Document};
use super::ParseError;
use crate::markup;

/// Reads an SRT file. Refuses one in which no block is a cue: an error page
/// served in its place must not read as an empty transcript.
pub fn parse(content: &str) -> Result<Document, ParseError> {
    let content = content.trim_start_matches('\u{feff}');
    let mut cues = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in content.lines().chain(std::iter::once("")) {
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
    if cues.is_empty() {
        return Err(ParseError::NotTimedText);
    }
    Ok(document_from(cues))
}

fn read_block(block: &[&str]) -> Option<Cue> {
    let timing_at = block.iter().take(2).position(|line| line.contains("-->"))?;
    let (begin_ms, end_ms) = parse_timing(block[timing_at])?;
    let payload = block[timing_at + 1..].join("\n");
    let text = markup::cue_to_text(&strip_ass_overrides(&payload));
    Some(Cue {
        begin_ms,
        end_ms,
        speaker: None,
        words: split_words(&markup::collapse(&text, MAX_CUE_CHARS)),
    })
}

/// `{\an8}` and its kind, left in many SRT files by the tools that made them.
/// Linear: a `{\` with no closing brace ends the search for good.
fn strip_ass_overrides(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{\\") {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        out.push_str(&rest[..open]);
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}
