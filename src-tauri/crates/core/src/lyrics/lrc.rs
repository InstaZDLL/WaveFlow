//! LRC and Enhanced LRC.
//!
//! `[mm:ss.xx]line` stamps a line; Enhanced LRC adds `<mm:ss.xx>` before
//! each word and one after the last, where the line ends. Neither has a
//! standard for singers or backing vocals, so those are left out on
//! export. Two conventions the wild uses are honoured both ways:
//!
//! - a timestamped **empty** line marks where the line before it ends,
//!   which is how plain LRC says "silence from here";
//! - a second line with the **same** timestamp is that line's
//!   translation, the way most LRC sites ship bilingual lyrics.

use crate::lyrics::model::{Document, Line, Translation, Word};
use crate::lyrics::resolve::{resolve, Units};
use crate::lyrics::time::{format_lrc_time, parse_lrc_time};
use crate::lyrics::ExportError;

/// Read LRC or Enhanced LRC. Lines without a timestamp are ignored, as
/// every player does; `[offset:]` is applied to every time and dropped.
pub fn parse(content: &str) -> Document {
    let mut doc = Document::default();
    let mut offset: i64 = 0;
    // (time, sequence) keeps lines with equal times in file order.
    let mut entries: Vec<(u64, usize, Line)> = Vec::new();
    let mut ends: Vec<u64> = Vec::new();

    for raw in content.trim_start_matches('\u{feff}').lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(after) = rest.strip_prefix('[') {
            let Some((tag, tail)) = after.split_once(']') else {
                break;
            };
            if let Some(t) = parse_lrc_time(tag) {
                times.push(t);
                rest = tail;
                continue;
            }
            if times.is_empty() {
                if let Some((key, value)) = tag.split_once(':') {
                    read_tag(&mut doc, &mut offset, key.trim(), value.trim());
                    rest = tail;
                    continue;
                }
            }
            break;
        }
        if times.is_empty() {
            continue;
        }
        let text = rest.trim();
        if text.is_empty() {
            ends.extend(times);
            continue;
        }
        let parsed = parse_word_stamps(text);
        for &t in &times {
            let mut line = Line {
                begin_ms: Some(t),
                ..Line::default()
            };
            match &parsed {
                Some((words, end)) => {
                    // A line stamped at several times repeats its word
                    // stamps relative to the first. One that cannot be
                    // held is no time.
                    let shift = |v: u64| v.checked_add(t).map(|s| s.saturating_sub(times[0]));
                    line.words = words
                        .iter()
                        .map(|w| Word {
                            begin_ms: w.begin_ms.and_then(shift),
                            end_ms: w.end_ms.and_then(shift),
                            ..w.clone()
                        })
                        .collect();
                    line.end_ms = end.and_then(shift);
                }
                None => line.words = crate::lyrics::model::split_words(text),
            }
            entries.push((t, entries.len(), line));
        }
    }

    entries.sort_by_key(|(t, seq, _)| (*t, *seq));
    if is_untimed(&entries) {
        // Text stamped all on one time is read as text, as the renderer
        // shows it: every line kept, in file order, none of it a
        // translation of the first.
        entries.sort_by_key(|(_, seq, _)| *seq);
        doc.lines = entries
            .into_iter()
            .map(|(_, _, line)| Line::from_text(&line.text()))
            .collect();
        return doc;
    }
    for (t, _, line) in entries {
        if let Some(prev) = doc.lines.last_mut() {
            if prev.begin_ms == Some(t)
                && line
                    .words
                    .iter()
                    .all(|w| w.begin_ms.is_none() || w.begin_ms == Some(t))
            {
                prev.translations.push(Translation {
                    lang: None,
                    text: line.text(),
                });
                continue;
            }
        }
        doc.lines.push(line);
    }
    for e in ends {
        if let Some(line) = doc
            .lines
            .iter_mut()
            .rev()
            // Strictly before: a line starting at that very stamp is the
            // one after the silence, not the one it ends.
            .find(|l| l.begin_ms.is_some_and(|b| b < e))
        {
            if line.end_ms.is_none() {
                line.end_ms = Some(e);
            }
        }
    }
    if offset != 0 {
        apply_offset(&mut doc, offset);
    }
    doc
}

/// Stamped text that is not timed: two lines or more, every one on the
/// same time — what sources that have no timing write (`[00:00.00]` on
/// each line), and what the renderer shows as plain text. Read as a line
/// and its translations, all but the first line would be lost to a save
/// in plain text. Unfolding loses nothing even when it guesses wrong (a
/// lone line with translations on its stamp keeps them as lines), so the
/// rule stays this narrow: counting lines per time would take the
/// translations of a short lyric for untimed text.
fn is_untimed(entries: &[(u64, usize, Line)]) -> bool {
    let has_word_stamps = entries
        .iter()
        .any(|(_, _, l)| l.words.iter().any(|w| w.begin_ms.is_some()));
    entries.len() >= 2 && !has_word_stamps && entries.iter().all(|(t, _, _)| *t == entries[0].0)
}

fn read_tag(doc: &mut Document, offset: &mut i64, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    let value = value.to_string();
    match key.to_ascii_lowercase().as_str() {
        "ti" => doc.metadata.title = Some(value),
        "ar" => doc.metadata.artist = Some(value),
        "al" => doc.metadata.album = Some(value),
        "au" => doc.metadata.songwriters.push(value),
        "la" | "lang" => doc.metadata.language = Some(value),
        "offset" => *offset = value.trim_start_matches('+').parse().unwrap_or(0),
        _ => {}
    }
}

/// `[offset:+500]` shows the lyrics 500 ms sooner. Worked out wide, so no
/// offset the file states can wrap a time; one pushed past what a time
/// can hold is dropped.
fn apply_offset(doc: &mut Document, offset: i64) {
    let shift = |v: &mut Option<u64>| {
        if let Some(t) = *v {
            *v = u64::try_from((i128::from(t) - i128::from(offset)).max(0)).ok();
        }
    };
    for line in &mut doc.lines {
        shift(&mut line.begin_ms);
        shift(&mut line.end_ms);
        for word in &mut line.words {
            shift(&mut word.begin_ms);
            shift(&mut word.end_ms);
        }
    }
}

/// Split Enhanced LRC text into stamped units. `None` when the text has no
/// word stamp at all. The second value is a trailing stamp with nothing
/// after it: where the line ends.
fn parse_word_stamps(text: &str) -> Option<(Vec<Word>, Option<u64>)> {
    let mut segments: Vec<(Option<u64>, &str)> = Vec::new();
    let mut current: Option<u64> = None;
    let mut start = 0;
    let mut search = 0;
    while let Some(rel) = text[search..].find('<') {
        let open = search + rel;
        let stamp = text[open + 1..].find('>').and_then(|close| {
            parse_lrc_time(&text[open + 1..open + 1 + close]).map(|t| (t, close))
        });
        match stamp {
            Some((t, close)) => {
                segments.push((current, &text[start..open]));
                current = Some(t);
                start = open + close + 2;
                search = start;
            }
            None => search = open + 1,
        }
    }
    segments.push((current, &text[start..]));
    if segments.iter().all(|(t, _)| t.is_none()) {
        return None;
    }

    let mut words: Vec<Word> = Vec::new();
    let mut line_end = None;
    let last = segments.len() - 1;
    for (k, (stamp, segment)) in segments.into_iter().enumerate() {
        if segment.trim().is_empty() {
            if let Some(prev) = words.last_mut() {
                if !segment.is_empty() {
                    prev.joined = false;
                }
                if let Some(t) = stamp {
                    if prev.end_ms.is_none() && (k == last || !segment.is_empty()) {
                        prev.end_ms = Some(t);
                    }
                }
            }
            if k == last {
                line_end = stamp;
            }
            continue;
        }
        if segment.starts_with(char::is_whitespace) {
            if let Some(prev) = words.last_mut() {
                prev.joined = false;
            }
        }
        let pieces: Vec<&str> = segment.split_whitespace().collect();
        for (p, piece) in pieces.iter().enumerate() {
            words.push(Word {
                text: (*piece).to_string(),
                begin_ms: if p == 0 { stamp } else { None },
                end_ms: None,
                joined: p + 1 == pieces.len() && !segment.ends_with(char::is_whitespace),
            });
        }
    }
    if let Some(w) = words.last_mut() {
        w.joined = false;
    }
    Some((words, line_end))
}

/// What to write.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WriteOptions {
    /// Enhanced LRC: stamp every word. Needs every word synced.
    pub enhanced: bool,
    /// `mm:ss.xxx` instead of the `mm:ss.xx` most players expect.
    pub millis: bool,
    /// Translations as same-timestamp lines.
    pub translations: bool,
    /// `[ti:]`, `[ar:]`, `[al:]`, `[au:]` header tags.
    pub metadata: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            enhanced: false,
            millis: false,
            translations: false,
            metadata: true,
        }
    }
}

/// Write LRC, or Enhanced LRC with [`WriteOptions::enhanced`].
pub fn write(doc: &Document, options: &WriteOptions) -> Result<String, ExportError> {
    let units = if options.enhanced {
        Units::Words
    } else {
        Units::Lines
    };
    let lines = resolve(doc, units)?;
    let stamp = |ms: u64| format_lrc_time(ms, options.millis);
    let mut out = String::new();

    if options.metadata {
        let m = &doc.metadata;
        for (key, value) in [("ti", &m.title), ("ar", &m.artist), ("al", &m.album)] {
            if let Some(v) = value {
                out.push_str(&format!("[{key}:{}]\n", one_line(v)));
            }
        }
        if !m.songwriters.is_empty() {
            out.push_str(&format!("[au:{}]\n", one_line(&m.songwriters.join(", "))));
        }
    }

    for (k, r) in lines.iter().enumerate() {
        out.push('[');
        out.push_str(&stamp(r.begin));
        out.push(']');
        match &r.words {
            Some(spans) => {
                for (word, span) in r.line.words.iter().zip(spans) {
                    out.push('<');
                    out.push_str(&stamp(span.begin));
                    out.push('>');
                    out.push_str(&word.text);
                    if !word.joined {
                        out.push(' ');
                    }
                }
                out.push('<');
                out.push_str(&stamp(r.end.unwrap_or(r.begin)));
                out.push('>');
            }
            None => out.push_str(&r.line.text()),
        }
        out.push('\n');
        if options.translations {
            for t in &r.line.translations {
                out.push_str(&format!("[{}]{}\n", stamp(r.begin), one_line(&t.text)));
            }
        }
        // Plain LRC can only say a line ended early with an empty line.
        if r.words.is_none() {
            let next = lines.get(k + 1).map(|n| n.begin);
            if let Some(end) = r.end {
                if next.map_or(true, |n| end < n) {
                    out.push_str(&format!("[{}]\n", stamp(end)));
                }
            }
        }
    }
    Ok(out)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plain_lrc_with_tags_and_offset() {
        let doc = parse("[ti:Song]\n[ar:Band]\n[offset:+200]\n[00:01.00]one two\nnot a lyric\n[00:03.00]three\n");
        assert_eq!(doc.metadata.title.as_deref(), Some("Song"));
        assert_eq!(doc.metadata.artist.as_deref(), Some("Band"));
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].begin_ms, Some(800));
        assert_eq!(doc.lines[0].text(), "one two");
        assert_eq!(doc.lines[1].begin_ms, Some(2800));
    }

    #[test]
    fn repeated_stamps_become_separate_lines_in_order() {
        let doc = parse("[00:10.00][00:01.00]hook\n[00:05.00]verse\n");
        let begins: Vec<_> = doc.lines.iter().map(|l| l.begin_ms.unwrap()).collect();
        assert_eq!(begins, vec![1000, 5000, 10_000]);
    }

    #[test]
    fn empty_stamp_ends_the_previous_line() {
        let doc = parse("[00:01.00]one\n[00:02.50]\n[00:04.00]two\n");
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].end_ms, Some(2500));
        assert_eq!(doc.lines[1].end_ms, None);
    }

    #[test]
    fn an_empty_stamp_ends_the_line_before_not_the_one_starting_there() {
        let doc = parse(
            "[00:01.00]one
[00:02.50]
[00:02.50]two
",
        );
        assert_eq!(doc.lines[0].end_ms, Some(2500));
        assert_eq!(doc.lines[1].end_ms, None);
    }

    #[test]
    fn text_stamped_all_on_one_time_keeps_every_line() {
        let doc = parse(
            "[00:00.00]one
[00:00.00]two
[00:00.00]three
",
        );
        let texts: Vec<String> = doc.lines.iter().map(Line::text).collect();
        assert_eq!(texts, ["one", "two", "three"]);
        assert!(doc.lines.iter().all(|l| l.begin_ms.is_none()));
        assert!(doc.lines.iter().all(|l| l.translations.is_empty()));
    }

    #[test]
    fn a_line_with_two_translations_round_trips() {
        let src = "[00:01.00]original
[00:01.00]first
[00:01.00]second
[00:04.00]next
";
        let doc = parse(src);
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].translations.len(), 2);
        let options = WriteOptions {
            translations: true,
            ..WriteOptions::default()
        };
        assert_eq!(write(&doc, &options).unwrap(), src);
    }

    #[test]
    fn stamps_too_large_to_hold_do_not_panic() {
        // A huge minute count is no stamp: the line is not read.
        assert!(parse(
            "[99999999999999999:00.00]x
"
        )
        .lines
        .is_empty());
        // Word stamps repeated on a second line time would overflow.
        let doc = parse(
            "[307445734561825:00.00][00:01.00]<307445734561825:00.00>x <307445734561825:00.50>
",
        );
        assert_eq!(doc.lines.len(), 2);
        // The offset is worked out wide: neither extreme wraps a time.
        let doc = parse(
            "[offset:-9223372036854775808]
[307445734561825:00.00]x
[00:02.00]y
",
        );
        assert_eq!(doc.lines[0].begin_ms, Some(9_223_372_036_854_777_808));
        assert_eq!(doc.lines[1].begin_ms, None);
        let doc = parse(
            "[offset:9223372036854775807]
[00:01.00]x
[00:02.00]y
",
        );
        assert_eq!(doc.lines[0].begin_ms, Some(0));
    }

    #[test]
    fn same_stamp_is_a_translation() {
        let doc = parse("[00:01.00]original\n[00:01.00]translated\n[00:02.00]next\n");
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[0].translations[0].text, "translated");
    }

    #[test]
    fn reads_word_stamps_and_syllables() {
        let doc =
            parse("[00:01.00]<00:01.00>beau<00:01.20>ti<00:01.40>ful <00:02.00>day <00:02.80>\n");
        let l = &doc.lines[0];
        assert_eq!(l.text(), "beautiful day");
        assert_eq!(l.words.len(), 4);
        assert!(l.words[0].joined && l.words[1].joined && !l.words[2].joined);
        assert_eq!(l.words[3].begin_ms, Some(2000));
        assert_eq!(l.words[3].end_ms, Some(2800));
        assert_eq!(l.end_ms, Some(2800));
    }

    #[test]
    fn plain_round_trip() {
        let src = "[ti:Song]\n[00:01.00]one two\n[00:02.50]\n[00:04.00]three\n";
        let out = write(&parse(src), &WriteOptions::default()).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn enhanced_round_trip() {
        let src = "[00:01.00]<00:01.00>beau<00:01.20>ti<00:01.40>ful <00:02.00>day <00:02.80>\n";
        let options = WriteOptions {
            enhanced: true,
            ..WriteOptions::default()
        };
        assert_eq!(write(&parse(src), &options).unwrap(), src);
    }

    #[test]
    fn enhanced_needs_every_word() {
        let doc = parse("[00:01.00]one two\n");
        let options = WriteOptions {
            enhanced: true,
            ..WriteOptions::default()
        };
        assert_eq!(
            write(&doc, &options).unwrap_err(),
            ExportError::UnsyncedWord { line: 0, word: 1 }
        );
    }

    #[test]
    fn translations_are_written_on_request() {
        // A second line on a time of its own: a lone line on one stamp is
        // read as untimed text (see `is_untimed`).
        let src = "[00:01.00]original\n[00:01.00]translated\n[00:03.00]next\n";
        let options = WriteOptions {
            translations: true,
            ..WriteOptions::default()
        };
        assert_eq!(write(&parse(src), &options).unwrap(), src);
    }
}
