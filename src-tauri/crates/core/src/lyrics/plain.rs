//! Unsynced text, one line per lyric line.

use crate::lyrics::model::{Document, Line};

/// Read plain text. A run of blank lines between two sung ones is a stanza
/// break and becomes one line without words: the timed formats skip it
/// (see [`crate::lyrics::resolve::resolve`]), and [`write`] puts it back.
/// Blank lines before the first sung line and after the last are dropped.
pub fn parse(content: &str) -> Document {
    let mut lines: Vec<Line> = Vec::new();
    let mut gap = false;
    for raw in content.trim_start_matches('\u{feff}').lines() {
        let text = raw.trim();
        if text.is_empty() {
            gap = !lines.is_empty();
            continue;
        }
        if gap {
            lines.push(Line::default());
            gap = false;
        }
        lines.push(Line::from_text(text));
    }
    Document {
        lines,
        ..Document::default()
    }
}

/// Write the sung text, one line per lyric line, without times. A line
/// without words between two sung ones is written as one blank line.
pub fn write(doc: &Document) -> String {
    let mut out = String::new();
    let mut gap = false;
    for line in &doc.lines {
        if line.words.is_empty() {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push('\n');
            gap = false;
        }
        out.push_str(&line.text());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stanza_breaks_survive_a_round_trip() {
        let doc = parse("\n\nfirst\nsecond\n\n\n\nthird\n\n");
        assert_eq!(doc.lines.len(), 4);
        assert!(doc.lines[2].words.is_empty());
        assert_eq!(write(&doc), "first\nsecond\n\nthird\n");
        // A break is no line to sync: the timed formats see three.
        assert!(!doc.is_line_synced());
    }
}
