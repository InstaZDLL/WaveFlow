//! Synced lyrics as a document: one model, and readers and writers for
//! the formats WaveFlow stores — plain text, LRC, Enhanced LRC and TTML.
//!
//! The lyrics editor holds a [`Document`] while the user types and syncs,
//! and writes it in the format they pick; the app's lyrics commands read
//! the result back like any other lyrics. What the *renderer* will follow
//! in a stored document (`lyrics_are_synced`, `lyrics_quality` in the app)
//! is a separate question with its own rules, because it mirrors the
//! frontend parser rather than this one.
//!
//! ```
//! use waveflow_core::lyrics::{lrc, ttml, Document};
//!
//! let doc = lrc::parse("[00:01.00]first line\n[00:03.50]second line\n");
//! assert!(doc.is_line_synced());
//! let xml = ttml::write(&doc, &ttml::WriteOptions::default()).unwrap();
//! assert_eq!(ttml::parse(&xml).unwrap().lines.len(), 2);
//! ```

pub mod lrc;
pub mod model;
pub mod plain;
pub mod resolve;
pub mod time;
pub mod ttml;

#[cfg(test)]
mod tests;

pub use model::{Agent, AgentKind, Document, Line, Metadata, Translation, Word};

/// Why a document could not be read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("not well-formed XML: {0}")]
    Xml(String),
    #[error("not a TTML document (root element is <{0}>)")]
    NotTtml(String),
}

/// Why a document could not be written in the format asked for.
///
/// Indexes count from zero and point into [`Document::lines`] and that
/// line's [`Line::words`], so the editor can jump to the culprit.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExportError {
    #[error("line {} has no start time", line + 1)]
    UnsyncedLine { line: usize },
    #[error("word {} of line {} has no start time", word + 1, line + 1)]
    UnsyncedWord { line: usize, word: usize },
}

/// The formats lyrics are stored in. Serialized as the app's lyrics
/// payloads and its `lyrics.format` column spell them (`enhanced_lrc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// Unsynced text.
    Plain,
    /// `[mm:ss.xx]`-prefixed lines.
    Lrc,
    /// LRC with `<mm:ss.xx>` word stamps
    /// (`[00:01.00]Hello <00:01.50>world`).
    EnhancedLrc,
    /// Apple-Music-style XML with `<span begin="…" end="…">` word timing.
    Ttml,
}

impl Format {
    /// Sniff the format of `content`.
    ///
    /// Order matters: TTML (XML envelope) is checked first because its
    /// `<p begin="...">` could otherwise look like nothing else, then
    /// Enhanced LRC (LRC with inline `<mm:ss.xx>` word stamps), then
    /// plain LRC, then unsynced text.
    pub fn detect(content: &str) -> Format {
        let content = content.trim_start_matches('\u{feff}');
        let head = content.trim_start();

        // TTML: XML declaration, root `<tt`, or the TTML namespace anywhere
        // in the first ~512 bytes. Apple Music's exported lyrics start with
        // `<?xml version="1.0"...`, LyricsX-style exports start with `<tt`.
        let head_lower_prefix: String = head
            .chars()
            .take(512)
            .collect::<String>()
            .to_ascii_lowercase();
        if head_lower_prefix.starts_with("<?xml")
            || head_lower_prefix.starts_with("<tt ")
            || head_lower_prefix.starts_with("<tt>")
            || head_lower_prefix.contains("xmlns=\"http://www.w3.org/ns/ttml\"")
            || head_lower_prefix.contains("<timedtext")
        {
            return Format::Ttml;
        }

        // Scan up to 40 lines (first lines may be `[ar:Artist]` / `[ti:…]`
        // LRC headers before the synced body starts).
        let mut has_line_stamp = false;
        let mut has_word_stamp = false;
        for raw in content.lines().take(40) {
            let line = raw.trim_start();
            // Line stamp: `[mm:ss` with both digits present.
            if line.starts_with('[')
                && line.len() >= 7
                && line[1..].chars().take(2).all(|c| c.is_ascii_digit())
                && line.as_bytes().get(3) == Some(&b':')
            {
                has_line_stamp = true;
                // Inline word stamp: `<mm:ss(.xx)?>` somewhere after the
                // first `]`. We scan the byte string directly to keep this
                // cheap for large libraries.
                if let Some(close) = line.find(']') {
                    let body = &line[close + 1..];
                    if word_stamp_present(body) {
                        has_word_stamp = true;
                        break;
                    }
                }
            }
        }

        if has_word_stamp {
            Format::EnhancedLrc
        } else if has_line_stamp {
            Format::Lrc
        } else {
            Format::Plain
        }
    }
}

/// Return true if `s` contains at least one `<\d+:\d+(\.\d+)?>` token —
/// the Enhanced LRC word-stamp shape. Hand-rolled (no regex dep) to
/// keep [`Format::detect`] allocation-free on the hot prefetch path.
pub fn word_stamp_present(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            // Need at least one digit, then ':', then one digit, then '>'.
            let digits1 = scan_digits(bytes, j);
            if digits1 > 0 {
                j += digits1;
                if bytes.get(j) == Some(&b':') {
                    j += 1;
                    let digits2 = scan_digits(bytes, j);
                    if digits2 > 0 {
                        j += digits2;
                        // Optional fractional `.xx` or `:xx`.
                        if matches!(bytes.get(j), Some(b'.') | Some(b':')) {
                            j += 1;
                            let frac = scan_digits(bytes, j);
                            j += frac;
                        }
                        if bytes.get(j) == Some(&b'>') {
                            return true;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

fn scan_digits(bytes: &[u8], start: usize) -> usize {
    let mut n = 0;
    while let Some(&b) = bytes.get(start + n) {
        if b.is_ascii_digit() {
            n += 1;
        } else {
            break;
        }
    }
    n
}

/// Read `content` in whatever format [`Format::detect`] finds it in.
pub fn parse_any(content: &str) -> Result<(Format, Document), ParseError> {
    let format = Format::detect(content);
    Ok((format, parse_as(content, format)?))
}

/// Read `content` as `format`. LRC and Enhanced LRC share one reader,
/// which takes word stamps wherever it finds them.
pub fn parse_as(content: &str, format: Format) -> Result<Document, ParseError> {
    Ok(match format {
        Format::Ttml => ttml::parse(content)?,
        Format::Lrc | Format::EnhancedLrc => lrc::parse(content),
        Format::Plain => plain::parse(content),
    })
}
