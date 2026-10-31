//! Turn a document's optional times into the concrete ones a writer needs.
//!
//! The editor lets a unit leave its end blank: tapping stamps only starts,
//! and a unit naturally ends where the next one begins. Writers that need
//! every bound (TTML, Enhanced LRC) get them from here, so they cannot
//! disagree about how a gap is filled.

use crate::lyrics::model::{Document, Line, Word};
use crate::lyrics::ExportError;

/// How long the very last unit of a song lasts when nothing says.
pub const DEFAULT_TAIL_MS: u64 = 2000;

/// Concrete bounds of one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub begin: u64,
    pub end: u64,
}

/// A line ready to be written.
#[derive(Debug, Clone)]
pub struct ResolvedLine<'a> {
    pub line: &'a Line,
    /// Position in [`Document::lines`], for error messages and keys.
    pub index: usize,
    pub begin: u64,
    /// `None` only for the last line of a line-timed document that never
    /// says when it ends; it then lasts until the song does.
    pub end: Option<u64>,
    /// One span per [`Line::words`] entry, when resolved with words.
    pub words: Option<Vec<Span>>,
    /// One span per [`Line::background`] entry, when every one of them
    /// has a start. Untimed backing vocals keep their text only.
    pub background: Option<Vec<Span>>,
}

/// Which lines are resolved down to their units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    /// None: every line is written with its own time only.
    Lines,
    /// Every line: a unit with no start refuses the export.
    Words,
    /// The lines whose units all have a start; the others are written
    /// with their own time only. A half-synced document keeps the word
    /// timing it has.
    WhereTimed,
}

/// Resolve every non-empty line. With [`Units::Words`], each unit must
/// have a start (the first one may borrow its line's) or the export is
/// refused, naming the first unit that has none.
pub fn resolve(doc: &Document, units: Units) -> Result<Vec<ResolvedLine<'_>>, ExportError> {
    let lines: Vec<(usize, &Line)> = doc
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.words.is_empty())
        .collect();
    let mut begins = Vec::with_capacity(lines.len());
    for (index, line) in &lines {
        begins.push(
            line.effective_begin()
                .ok_or(ExportError::UnsyncedLine { line: *index })?,
        );
    }

    let mut out = Vec::with_capacity(lines.len());
    for (k, (index, line)) in lines.iter().enumerate() {
        let begin = begins[k];
        let next_begin = begins.get(k + 1).copied();
        let explicit_end = line
            .end_ms
            .or_else(|| line.words.last().and_then(|w| w.end_ms))
            .into_iter()
            .chain(line.background.last().and_then(|w| w.end_ms))
            .max();
        let mut end = explicit_end.or(next_begin);

        let starts = match units {
            Units::Lines => None,
            Units::Words => Some(
                word_starts(&line.words, Some(begin))
                    .ok_or_else(|| first_untimed(*index, &line.words))?,
            ),
            Units::WhereTimed => word_starts(&line.words, Some(begin)),
        };
        let words = if let Some(starts) = starts {
            // A stated end before the last unit starts is stale (that
            // unit was stamped again after it): the line runs on as if
            // it stated none, or its last unit would never sound.
            if end.zip(starts.last()).is_some_and(|(e, &s)| e < s) {
                end = next_begin;
            }
            let spans = spans(&line.words, &starts, end);
            if end.is_none() {
                end = spans.last().map(|s| s.end);
            }
            Some(spans)
        } else {
            None
        };
        let background = word_starts(&line.background, None)
            .filter(|_| units != Units::Lines && !line.background.is_empty())
            .map(|starts| spans(&line.background, &starts, end));
        if let (Some(e), Some(bg)) = (end, &background) {
            end = Some(e.max(bg.last().map_or(e, |s| s.end)));
        }

        out.push(ResolvedLine {
            line,
            index: *index,
            begin,
            end: end.map(|e| e.max(begin)),
            words,
            background,
        });
    }
    Ok(out)
}

fn word_starts(words: &[Word], first_fallback: Option<u64>) -> Option<Vec<u64>> {
    words
        .iter()
        .enumerate()
        .map(|(j, w)| w.begin_ms.or(if j == 0 { first_fallback } else { None }))
        .collect()
}

fn first_untimed(line: usize, words: &[Word]) -> ExportError {
    let word = words
        .iter()
        .skip(1)
        .position(|w| w.begin_ms.is_none())
        .map_or(0, |p| p + 1);
    ExportError::UnsyncedWord { line, word }
}

fn spans(words: &[Word], starts: &[u64], line_end: Option<u64>) -> Vec<Span> {
    (0..words.len())
        .map(|j| {
            let begin = starts[j];
            let end = words[j]
                .end_ms
                .or_else(|| starts.get(j + 1).copied())
                .or(line_end)
                .unwrap_or(begin + DEFAULT_TAIL_MS);
            Span {
                begin,
                end: end.max(begin),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::model::Word;

    fn line(words: Vec<Word>) -> Line {
        Line {
            words,
            ..Line::default()
        }
    }

    #[test]
    fn a_unit_ends_where_the_next_begins() {
        let doc = Document {
            lines: vec![
                line(vec![
                    Word::timed("a", 1000, None),
                    Word::timed("b", 1500, None),
                ]),
                line(vec![Word::timed("c", 3000, Some(3400))]),
            ],
            ..Document::default()
        };
        let r = resolve(&doc, Units::Words).unwrap();
        let w = r[0].words.as_ref().unwrap();
        assert_eq!(
            w[0],
            Span {
                begin: 1000,
                end: 1500
            }
        );
        assert_eq!(
            w[1],
            Span {
                begin: 1500,
                end: 3000
            }
        );
        assert_eq!(r[0].end, Some(3000));
        assert_eq!(r[1].end, Some(3400));
    }

    #[test]
    fn the_last_unit_of_the_song_gets_a_default_tail() {
        let doc = Document {
            lines: vec![line(vec![Word::timed("a", 1000, None)])],
            ..Document::default()
        };
        let r = resolve(&doc, Units::Words).unwrap();
        assert_eq!(r[0].words.as_ref().unwrap()[0].end, 1000 + DEFAULT_TAIL_MS);
        assert_eq!(r[0].end, Some(1000 + DEFAULT_TAIL_MS));
        // Line timing alone does not invent an end.
        assert_eq!(resolve(&doc, Units::Lines).unwrap()[0].end, None);
    }

    #[test]
    fn the_first_word_borrows_the_line_start() {
        let mut l = line(vec![Word::new("a"), Word::timed("b", 1200, None)]);
        l.begin_ms = Some(1000);
        let doc = Document {
            lines: vec![l],
            ..Document::default()
        };
        let r = resolve(&doc, Units::Words).unwrap();
        assert_eq!(r[0].words.as_ref().unwrap()[0].begin, 1000);
    }

    #[test]
    fn untimed_units_are_named() {
        let doc = Document {
            lines: vec![
                line(vec![Word::timed("a", 0, None)]),
                line(vec![Word::timed("b", 900, None), Word::new("c")]),
            ],
            ..Document::default()
        };
        assert_eq!(
            resolve(&doc, Units::Words).unwrap_err(),
            ExportError::UnsyncedWord { line: 1, word: 1 }
        );
        let doc = Document::from_plain_text("x");
        assert_eq!(
            resolve(&doc, Units::Lines).unwrap_err(),
            ExportError::UnsyncedLine { line: 0 }
        );
    }

    #[test]
    fn a_stale_line_end_does_not_cut_the_last_unit() {
        let mut l = line(vec![
            Word::timed("a", 1000, None),
            Word::timed("b", 2000, None),
        ]);
        // Ended at 1500, then `b` was stamped again at 2000.
        l.end_ms = Some(1500);
        let doc = Document {
            lines: vec![l, line(vec![Word::timed("c", 3000, None)])],
            ..Document::default()
        };
        let r = resolve(&doc, Units::Words).unwrap();
        assert_eq!(
            r[0].words.as_ref().unwrap()[1],
            Span {
                begin: 2000,
                end: 3000
            }
        );
        assert_eq!(r[0].end, Some(3000));
    }

    #[test]
    fn where_timed_keeps_the_words_of_the_lines_that_have_them() {
        let mut half = line(vec![Word::new("c"), Word::new("d")]);
        half.begin_ms = Some(3000);
        let doc = Document {
            lines: vec![
                line(vec![
                    Word::timed("a", 1000, None),
                    Word::timed("b", 1500, None),
                ]),
                half,
            ],
            ..Document::default()
        };
        let r = resolve(&doc, Units::WhereTimed).unwrap();
        assert_eq!(r[0].words.as_ref().map(Vec::len), Some(2));
        assert!(r[1].words.is_none());
        assert_eq!(r[1].begin, 3000);
    }

    #[test]
    fn backing_vocals_can_extend_the_line() {
        let mut l = line(vec![Word::timed("a", 1000, Some(1500))]);
        l.background = vec![Word::timed("(b)", 1400, Some(2200))];
        let doc = Document {
            lines: vec![l],
            ..Document::default()
        };
        let r = resolve(&doc, Units::Words).unwrap();
        assert_eq!(r[0].end, Some(2200));
        assert_eq!(r[0].background.as_ref().unwrap()[0].end, 2200);
    }
}
