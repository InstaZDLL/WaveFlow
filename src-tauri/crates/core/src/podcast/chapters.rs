//! An episode's chapters, from whichever source has them.
//!
//! A chapter is a start time and a title, sorted, with impossible times
//! dropped. A chapter's image and link are ignored on purpose: each would be
//! one more address out of a feed for something to open.
//!
//! What is stored is read again on the way out ([`read_cache`], [`read_stored`]):
//! a row may have come from an archive somebody made.

use serde::{Deserialize, Serialize};

use super::{MAX_CHAPTERS_BYTES, MAX_PLAUSIBLE_MS};

/// How many chapters one episode keeps.
pub(super) const MAX_CHAPTERS: usize = 2_000;

/// The longest title kept, in characters.
const MAX_TITLE_CHARS: usize = 300;

/// The largest cached list read. A full list of the longest titles, in the widest
/// characters, fits; anything longer was not written here.
const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;

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
    /// `CHAP` frames in an MP3, from its file or the start of its stream.
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

/// A list as it is kept with its episode.
pub fn write_cache(source: ChapterSource, chapters: &[Chapter]) -> Option<String> {
    serde_json::to_string(&CachedRef { source, chapters }).ok()
}

/// A kept list, read as what it is: something a database says. `None`, which means
/// it is to be found again, when it is not one of ours or holds no valid chapter.
pub fn read_cache(stored: &str) -> Option<(ChapterSource, Vec<Chapter>)> {
    if stored.len() > MAX_CACHE_BYTES {
        return None;
    }
    let cached: Cached = serde_json::from_str(stored).ok()?;
    let chapters = revalidated(cached.chapters);
    (!chapters.is_empty()).then_some((cached.source, chapters))
}

/// The `psc:chapters` of a row, read again for the same reason.
pub fn read_stored(stored: &str) -> Vec<Chapter> {
    if stored.len() > MAX_CACHE_BYTES {
        return Vec::new();
    }
    serde_json::from_str(stored).map_or_else(|_| Vec::new(), revalidated)
}

fn revalidated(mut chapters: Vec<Chapter>) -> Vec<Chapter> {
    chapters.truncate(MAX_CHAPTERS * 2);
    for chapter in &mut chapters {
        chapter.title = crate::markup::collapse(&chapter.title, MAX_TITLE_CHARS);
    }
    normalize(chapters, None)
}

#[derive(Serialize)]
struct CachedRef<'a> {
    source: ChapterSource,
    chapters: &'a [Chapter],
}

#[derive(Deserialize)]
struct Cached {
    source: ChapterSource,
    chapters: Vec<Chapter>,
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

pub(super) fn clean_title(raw: &str) -> String {
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
