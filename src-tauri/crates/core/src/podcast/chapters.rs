//! An episode's chapters, from whichever source has them.
//!
//! A chapter is a start time and a title, sorted, with impossible times
//! dropped. A chapter's image and link are ignored on purpose: each would be
//! one more address out of a feed for something to open.

use serde::{Deserialize, Serialize};

use super::{MAX_CHAPTERS_BYTES, MAX_PLAUSIBLE_MS};

/// How many chapters one episode keeps.
const MAX_CHAPTERS: usize = 2_000;

/// The longest title kept, in characters.
const MAX_TITLE_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub start_ms: u64,
    pub title: String,
}

/// Where the chapters that won came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChapterSource {
    /// The feed's `podcast:chapters` JSON file.
    Json,
    /// Inline `psc:chapters`.
    Inline,
    /// `CHAP` frames in a downloaded MP3.
    Id3,
}

/// The chapters to show: the first source, in the order given, with at least
/// one valid chapter. Sources are not merged, since two of them rarely agree
/// about where a chapter starts and a merged list would show both opinions.
pub fn choose(
    json: Option<Vec<Chapter>>,
    inline: Vec<Chapter>,
    id3: Vec<Chapter>,
) -> Option<(ChapterSource, Vec<Chapter>)> {
    [
        (ChapterSource::Json, json.unwrap_or_default()),
        (ChapterSource::Inline, inline),
        (ChapterSource::Id3, id3),
    ]
    .into_iter()
    .map(|(source, chapters)| (source, normalize(chapters, None)))
    .find(|(_, chapters)| !chapters.is_empty())
}

/// Sorted by start, one per start, none untitled or past `duration_ms` when it
/// is known, at most [`MAX_CHAPTERS`].
///
/// The first of two at the same time stays. A feed's declared duration is not
/// what `duration_ms` is for, since feeds get it wrong: pass what the stream
/// measured.
pub fn normalize(mut chapters: Vec<Chapter>, duration_ms: Option<u64>) -> Vec<Chapter> {
    chapters.retain(|chapter| {
        !chapter.title.is_empty()
            && chapter.start_ms
                < duration_ms
                    .unwrap_or(MAX_PLAUSIBLE_MS)
                    .min(MAX_PLAUSIBLE_MS)
    });
    chapters.sort_by_key(|chapter| chapter.start_ms);
    chapters.dedup_by_key(|chapter| chapter.start_ms);
    chapters.truncate(MAX_CHAPTERS);
    chapters
}

/// A Podcasting 2.0 chapter file (`{"chapters": [{"startTime": 0, "title": …}]}`).
/// `None` when this is not one; `Some` of nothing when it holds no valid
/// chapter, which is the case [`choose`] moves on from.
pub fn read_json(bytes: &[u8]) -> Option<Vec<Chapter>> {
    if bytes.len() > MAX_CHAPTERS_BYTES {
        return None;
    }
    // `serde_json` stops at its recursion limit, so a nesting of any depth is
    // a refusal and not a stack overflow.
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let chapters = value.get("chapters")?.as_array()?;
    Some(normalize(
        chapters
            .iter()
            .filter(|chapter| chapter.get("toc").and_then(|toc| toc.as_bool()) != Some(false))
            .filter_map(|chapter| {
                let seconds = chapter.get("startTime")?.as_f64()?;
                Some(Chapter {
                    start_ms: seconds_to_ms(seconds)?,
                    title: clean_title(chapter.get("title")?.as_str()?),
                })
            })
            .collect(),
        None,
    ))
}

/// The children of a `psc:chapters` element.
pub(super) fn read_inline(node: roxmltree::Node<'_, '_>) -> Vec<Chapter> {
    normalize(
        node.children()
            .filter(|child| child.is_element() && child.tag_name().name() == "chapter")
            .filter_map(|child| {
                Some(Chapter {
                    start_ms: parse_clock(child.attribute("start")?)?,
                    title: clean_title(child.attribute("title")?),
                })
            })
            .collect(),
        None,
    )
}

fn clean_title(raw: &str) -> String {
    crate::markup::html_to_line(raw, MAX_TITLE_CHARS)
}

fn seconds_to_ms(seconds: f64) -> Option<u64> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    let ms = (seconds * 1000.0).round();
    (ms < MAX_PLAUSIBLE_MS as f64).then_some(ms as u64)
}

/// `HH:MM:SS.mmm`, `MM:SS` or plain seconds, the spellings `psc:chapter` allows.
fn parse_clock(value: &str) -> Option<u64> {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let (last, whole) = parts.split_last()?;
    let mut seconds = 0.0_f64;
    for part in whole {
        let number: u32 = part.trim().parse().ok()?;
        seconds = seconds * 60.0 + f64::from(number);
    }
    let last: f64 = last.trim().parse().ok()?;
    if !last.is_finite() || last < 0.0 {
        return None;
    }
    seconds_to_ms(seconds * 60.0 + last)
}
