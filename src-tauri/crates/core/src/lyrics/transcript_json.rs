//! A Podcasting 2.0 JSON transcript, as a [`Document`]. Reader only.
//!
//! The format is a list of segments, each a start, an end, a speaker and a
//! body. A body may be a sentence or a single word: several publishers write
//! one segment per word, and a panel that follows lines would show one word at
//! a time. So runs of one-word segments are gathered into lines, on the same
//! [`Word`]s the karaoke renderer already times.

use serde_json::Value;

use super::cue::{document_from, Cue, MAX_CUES, MAX_CUE_CHARS};
use super::model::{split_words, Document, Word};
use super::ParseError;
use crate::markup;

/// How many words a gathered line holds at most.
const MAX_WORDS_PER_LINE: usize = 14;

/// The silence that ends a gathered line.
const GAP_ENDS_LINE_MS: u64 = 1_500;

/// The latest time believed: 100 hours.
const MAX_TIME_MS: f64 = 100.0 * 3_600_000.0;

struct Segment {
    begin_ms: u64,
    end_ms: Option<u64>,
    speaker: Option<String>,
    body: String,
}

/// Reads a transcript. Refuses JSON that is not one; a segment with an
/// impossible time or no body is skipped.
pub fn parse(content: &str) -> Result<Document, ParseError> {
    // `serde_json` stops at its recursion limit, so a nesting of any depth is
    // a refusal and not a stack overflow.
    let value: Value = serde_json::from_str(content.trim_start_matches('\u{feff}'))
        .map_err(|_| ParseError::NotTimedText)?;
    let segments = value
        .get("segments")
        .and_then(Value::as_array)
        .ok_or(ParseError::NotTimedText)?;
    let mut segments: Vec<Segment> = segments
        .iter()
        .take(MAX_CUES)
        .filter_map(read_segment)
        .collect();
    segments.sort_by_key(|segment| segment.begin_ms);
    Ok(document_from(gather(segments)))
}

fn read_segment(segment: &Value) -> Option<Segment> {
    let begin_ms = seconds_to_ms(segment.get("startTime")?.as_f64()?)?;
    let end_ms = segment
        .get("endTime")
        .and_then(Value::as_f64)
        .and_then(seconds_to_ms)
        .filter(|end| *end > begin_ms);
    let body = markup::collapse(
        &markup::cue_to_text(segment.get("body")?.as_str()?),
        MAX_CUE_CHARS,
    );
    if body.is_empty() {
        return None;
    }
    Some(Segment {
        begin_ms,
        end_ms,
        speaker: segment
            .get("speaker")
            .and_then(Value::as_str)
            .map(str::to_owned),
        body,
    })
}

fn seconds_to_ms(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    let ms = (seconds * 1000.0).round();
    (ms < MAX_TIME_MS).then_some(ms as u64)
}

fn gather(segments: Vec<Segment>) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut open = false;
    for segment in segments {
        if segment.body.contains(' ') {
            cues.push(Cue {
                begin_ms: segment.begin_ms,
                end_ms: segment.end_ms,
                speaker: segment.speaker,
                words: split_words(&segment.body),
            });
            open = false;
            continue;
        }
        let word_end = segment.end_ms.unwrap_or(segment.begin_ms);
        // Punctuation written as a segment of its own belongs to the word
        // before it, and a line shows no lone comma.
        let only_punctuation = !segment.body.chars().any(char::is_alphanumeric);
        let continues = cues.last().filter(|_| open).is_some_and(|cue| {
            cue.speaker == segment.speaker
                && (only_punctuation
                    || (cue.words.len() < MAX_WORDS_PER_LINE
                        && segment
                            .begin_ms
                            .saturating_sub(cue.end_ms.unwrap_or(cue.begin_ms))
                            < GAP_ENDS_LINE_MS))
        });
        let ends_sentence = segment.body.ends_with(['.', '?', '!', '…']);
        if continues {
            let cue = cues.last_mut().expect("an open line exists");
            if only_punctuation {
                if let Some(last) = cue.words.last_mut() {
                    last.text.push_str(&segment.body);
                }
            } else {
                cue.words
                    .push(Word::timed(segment.body, segment.begin_ms, segment.end_ms));
            }
            cue.end_ms = cue.end_ms.max(Some(word_end));
        } else {
            cues.push(Cue {
                begin_ms: segment.begin_ms,
                end_ms: segment.end_ms,
                speaker: segment.speaker,
                words: vec![Word::timed(segment.body, segment.begin_ms, segment.end_ms)],
            });
        }
        open = !ends_sentence;
    }
    cues
}
