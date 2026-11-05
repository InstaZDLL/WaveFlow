//! What the timed-text readers share: a cue, its clock, and the document a
//! list of cues becomes.
//!
//! These read a podcast's transcript, a hostile document up to several
//! megabytes long, so a cue's time is checked rather than trusted and the
//! whole is bounded. Cues may come in any order and the renderer follows lines
//! by their start, so they are sorted here.

use super::model::{Agent, AgentKind, Document, Line, Word};

/// How many cues one document keeps. A line every second for fourteen hours.
pub(crate) const MAX_CUES: usize = 50_000;

/// The longest text of a cue kept, in characters.
pub(crate) const MAX_CUE_CHARS: usize = 2_000;

/// How many distinct voices one document names.
const MAX_SPEAKERS: usize = 200;

/// The longest speaker's name kept, in characters.
const MAX_SPEAKER_CHARS: usize = 100;

/// The latest time believed: 100 hours.
const MAX_TIME_MS: u64 = 100 * 3_600_000;

/// One timed run of text, before it is a [`Line`].
pub(crate) struct Cue {
    pub begin_ms: u64,
    pub end_ms: Option<u64>,
    pub speaker: Option<String>,
    pub words: Vec<Word>,
}

/// `HH:MM:SS.mmm` or `MM:SS.mmm`, with `.` or `,` before the fraction. Minutes
/// and seconds out of range, or a time past [`MAX_TIME_MS`], are no time.
pub(crate) fn parse_clock(value: &str) -> Option<u64> {
    let (whole, fraction) = match value.trim().split_once(['.', ',']) {
        Some((whole, fraction)) => (whole, fraction),
        None => (value.trim(), ""),
    };
    let parts: Vec<&str> = whole.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => ("0", *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    let hours = digits(hours)?;
    let minutes = digits(minutes)?;
    let seconds = digits(seconds)?;
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    let millis = if fraction.is_empty() {
        0
    } else {
        // The first three digits: `.5` is 500 ms, and a longer fraction than a
        // millisecond is more than anything here can use.
        let head: String = fraction.chars().chain("00".chars()).take(3).collect();
        digits(&head)?
    };
    let total = hours
        .checked_mul(3_600_000)?
        .checked_add(minutes * 60_000)?
        .checked_add(seconds * 1000)?
        .checked_add(millis)?;
    (total < MAX_TIME_MS).then_some(total)
}

fn digits(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// A timing line (`00:01.000 --> 00:02.000 align:start`): start, and an end
/// that is only kept when it comes after the start.
pub(crate) fn parse_timing(line: &str) -> Option<(u64, Option<u64>)> {
    let (begin, rest) = line.split_once("-->")?;
    let begin = parse_clock(begin)?;
    let end = rest
        .split_whitespace()
        .next()
        .and_then(parse_clock)
        .filter(|end| *end > begin);
    Some((begin, end))
}

/// `cues` as a document: sorted by start, the first of a tie staying first,
/// with a speaker's name turned into an [`Agent`] the lines refer to.
pub(crate) fn document_from(mut cues: Vec<Cue>) -> Document {
    cues.retain(|cue| !cue.words.is_empty());
    cues.truncate(MAX_CUES);
    cues.sort_by_key(|cue| cue.begin_ms);
    let mut agents: Vec<Agent> = Vec::new();
    let lines = cues
        .into_iter()
        .map(|cue| Line {
            begin_ms: Some(cue.begin_ms),
            end_ms: cue.end_ms,
            agent: cue
                .speaker
                .as_deref()
                .and_then(|name| agent_id(&mut agents, name)),
            words: cue.words,
            ..Line::default()
        })
        .collect();
    Document {
        agents,
        lines,
        ..Document::default()
    }
}

/// The id of the agent called `name`, added when it is new. Past
/// [`MAX_SPEAKERS`] a new voice stays unnamed.
fn agent_id(agents: &mut Vec<Agent>, name: &str) -> Option<String> {
    let name = crate::markup::collapse(name, MAX_SPEAKER_CHARS);
    if name.is_empty() {
        return None;
    }
    if let Some(known) = agents.iter().find(|a| a.name.as_deref() == Some(&name)) {
        return Some(known.id.clone());
    }
    if agents.len() >= MAX_SPEAKERS {
        return None;
    }
    let id = format!("v{}", agents.len() + 1);
    agents.push(Agent {
        id: id.clone(),
        kind: AgentKind::Person,
        name: Some(name),
    });
    Some(id)
}

#[cfg(test)]
mod tests;
