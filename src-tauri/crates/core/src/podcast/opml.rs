//! A subscription list as OPML: read when someone brings one, written when
//! they ask for theirs.
//!
//! The writer carries feed addresses, and that is the point of it: exporting
//! is a deliberate act, and the dialog that does it says so. Automatic backups
//! are the other path, and they carry none.

use super::address::parse_address;
use super::feed::{decode, parse_xml};

/// How many feeds one file brings. Far more than anyone follows, and each one
/// is a request.
const MAX_OPML_ENTRIES: usize = 2_000;

/// The longest title kept, in characters.
const MAX_TITLE_CHARS: usize = 500;

/// One subscription in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpmlEntry {
    pub title: Option<String>,
    pub feed_url: String,
}

/// The feeds a document lists, however its folders nest them, in document
/// order and each address once. An outline without a usable address is a
/// folder or something else, and is skipped. `None` when this is not OPML.
pub fn read_opml(bytes: &[u8]) -> Option<Vec<OpmlEntry>> {
    let text = decode(bytes)?;
    let document = parse_xml(&text)?;
    let root = document.root_element();
    if root.tag_name().name() != "opml" {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    Some(
        // `descendants` is iterative: a nesting of any depth costs no stack.
        root.descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "outline")
            .filter_map(|node| {
                let raw = node
                    .attribute("xmlUrl")
                    .or_else(|| node.attribute("xmlurl"))?;
                let feed_url = parse_address(raw)?.to_string();
                let title = node
                    .attribute("title")
                    .or_else(|| node.attribute("text"))
                    .map(|title| crate::markup::html_to_line(title, MAX_TITLE_CHARS))
                    .filter(|title| !title.is_empty());
                Some(OpmlEntry { title, feed_url })
            })
            .filter(|entry| seen.insert(entry.feed_url.clone()))
            .take(MAX_OPML_ENTRIES)
            .collect(),
    )
}

/// `entries` as an OPML 2.0 document.
pub fn write_opml(entries: &[OpmlEntry]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <opml version=\"2.0\">\n  <head><title>WaveFlow podcasts</title></head>\n  <body>\n",
    );
    for entry in entries {
        let title = escape_attribute(entry.title.as_deref().unwrap_or_default());
        let url = escape_attribute(&entry.feed_url);
        out.push_str(&format!(
            "    <outline type=\"rss\" text=\"{title}\" title=\"{title}\" xmlUrl=\"{url}\"/>\n"
        ));
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

/// A string as an XML attribute's value. What XML 1.0 cannot hold at all, the
/// control characters and the two non-characters, is dropped: a title stored
/// with one must not make the whole list unreadable for whoever imports it.
fn escape_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(' '),
            '\u{FFFE}' | '\u{FFFF}' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}
