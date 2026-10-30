//! Unsynced text, one line per lyric line.

use crate::lyrics::model::Document;

/// Read plain text. Blank lines (stanza breaks) are dropped: no format we
/// write can carry them, and a timed blank line would only light nothing.
pub fn parse(content: &str) -> Document {
    Document::from_plain_text(content.trim_start_matches('\u{feff}'))
}

/// Write the sung text, one line per lyric line, without times.
pub fn write(doc: &Document) -> String {
    let mut out = String::new();
    for line in doc.lines.iter().filter(|l| !l.words.is_empty()) {
        out.push_str(&line.text());
        out.push('\n');
    }
    out
}
