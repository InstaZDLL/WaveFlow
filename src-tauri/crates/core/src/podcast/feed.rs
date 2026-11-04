//! A channel and its episodes, read out of RSS 2.0, Atom or RSS 1.0.
//!
//! Parsing goes through `roxmltree` with a DTD refused outright, so an entity
//! cannot be declared, let alone expanded, and with a node ceiling. The bytes
//! are bounded before they are decoded, and every text kept afterwards. A
//! parser's own message would quote the document, so a refusal is one value
//! ([`FeedError`]) and carries nothing.

use roxmltree::Node;
use url::Url;

use super::address::{parse_address, MAX_URL_CHARS};
use super::chapters::{self, Chapter};
use super::transcript::{TranscriptKind, TranscriptRef};
use super::{MAX_FEED_BYTES, MAX_PLAUSIBLE_MS};
use crate::markup;

/// How many nodes a feed may parse into. A long-running show's feed holds a
/// few thousand items of a dozen nodes each; this is far above that and far
/// below what would exhaust memory.
const MAX_FEED_NODES: u32 = 1_000_000;

/// How many episodes one refresh keeps from a feed, newest first.
pub const MAX_EPISODES_PER_FEED: usize = 500;

/// The longest text kept from a title, in characters.
const MAX_TITLE_CHARS: usize = 500;

/// The longest text kept from a description, in characters.
const MAX_DESCRIPTION_CHARS: usize = 4_000;

/// How many transcripts one episode keeps a reference to.
const MAX_TRANSCRIPTS_PER_EPISODE: usize = 16;

/// A date is believed between these years: a feed's `9999` must not hold the
/// top of a list for ever, and one before podcasts is a typo.
const EARLIEST_YEAR_MS: i64 = 631_152_000_000; // 1990-01-01
const LATEST_YEAR_MS: i64 = 4_102_444_800_000; // 2100-01-01

/// A channel, as its feed describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Feed {
    /// Empty when the feed names none: the page shows its own word for that,
    /// which a parser with no locale could not choose.
    pub title: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub language: Option<String>,
    pub site_url: Option<String>,
    pub image_url: Option<String>,
    /// Newest first, at most [`MAX_EPISODES_PER_FEED`].
    pub episodes: Vec<FeedEpisode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeedEpisode {
    /// The item's guid, or its enclosure's address when it has none. May hold
    /// a private token: see [`item_key`](super::address::item_key).
    pub guid: String,
    /// Empty when the item names none, as for [`Feed::title`].
    pub title: String,
    pub description: Option<String>,
    pub published_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub season: Option<i64>,
    pub episode_number: Option<i64>,
    pub enclosure_url: String,
    pub enclosure_type: Option<String>,
    pub enclosure_length: Option<i64>,
    /// `podcast:chapters`, a JSON file fetched when the episode is opened.
    pub chapters_url: Option<String>,
    /// `psc:chapters`, already valid and sorted.
    pub inline_chapters: Vec<Chapter>,
    /// `podcast:transcript`, in the order the feed gave them.
    pub transcripts: Vec<TranscriptRef>,
}

/// Why a body is not a feed. One word is all that is stored: the parser's own
/// message would quote the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedError {
    /// Too large, not text, not XML, a DTD, too many nodes, or neither RSS
    /// nor Atom.
    Invalid,
}

/// Reads an RSS 2.0, Atom or RSS 1.0 document.
///
/// `base` is where the document was fetched from, against which a relative
/// address is resolved. An item without a usable audio enclosure is not an
/// episode and is skipped. When two items share a guid the newer one is kept.
pub fn parse_feed(bytes: &[u8], base: &Url) -> Result<Feed, FeedError> {
    let text = decode(bytes).ok_or(FeedError::Invalid)?;
    let document = parse_xml(&text).ok_or(FeedError::Invalid)?;
    let root = document.root_element();
    let mut feed = match root.tag_name().name() {
        "rss" => {
            let channel = child(root, "channel").ok_or(FeedError::Invalid)?;
            parse_rss(channel, base)
        }
        "feed" => parse_atom(root, base),
        // RSS 1.0 (RDF): channel and items are siblings under the root.
        "RDF" => parse_rdf(root, base),
        _ => return Err(FeedError::Invalid),
    };
    feed.episodes
        .sort_by_key(|episode| std::cmp::Reverse(episode.published_at));
    let mut seen = std::collections::HashSet::new();
    feed.episodes
        .retain(|episode| seen.insert(episode.guid.clone()));
    feed.episodes.truncate(MAX_EPISODES_PER_FEED);
    Ok(feed)
}

/// `text` as a tree, under the rules every XML document here is read by.
pub(super) fn parse_xml(text: &str) -> Option<roxmltree::Document<'_>> {
    let options = roxmltree::ParsingOptions {
        // The default, written down because it is the protection: a document
        // declaring a DTD is refused, so no entity can be defined to expand.
        allow_dtd: false,
        nodes_limit: MAX_FEED_NODES,
        ..roxmltree::ParsingOptions::default()
    };
    roxmltree::Document::parse_with_options(text, options).ok()
}

/// The document as text: a byte-order mark first, then the encoding the XML
/// declaration names, then UTF-8. Bytes that are not what the document says
/// they are refuse it: guessing would store mojibake as a title.
pub(super) fn decode(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_FEED_BYTES {
        return None;
    }
    if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, malformed) = encoding.decode(bytes);
        return (!malformed).then(|| text.into_owned());
    }
    let encoding = declared_encoding(bytes)
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, malformed) = encoding.decode_without_bom_handling(bytes);
    (!malformed).then(|| text.into_owned())
}

/// The label in `<?xml … encoding="…"?>`, if the document opens with one.
fn declared_encoding(bytes: &[u8]) -> Option<String> {
    // Lossy, because only the ASCII of the declaration matters here and the
    // cut at 256 bytes may fall inside a character.
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
    let declaration = head.trim_start().strip_prefix("<?xml")?;
    let declaration = &declaration[..declaration.find("?>")?];
    let after = declaration[declaration.find("encoding")? + "encoding".len()..].trim_start();
    let after = after.strip_prefix('=')?.trim_start();
    let quote = after.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let rest = &after[1..];
    Some(rest[..rest.find(quote)?].to_owned())
}

pub(super) fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|candidate| candidate.is_element() && candidate.tag_name().name() == name)
}

/// The text of the first child called `name`, in whatever namespace, that has
/// any, as one bounded line.
fn text_of(node: Node<'_, '_>, name: &str, limit: usize) -> Option<String> {
    node.children()
        .filter(|candidate| candidate.is_element() && candidate.tag_name().name() == name)
        .map(|candidate| collect_text(candidate, limit))
        .find(|text| !text.is_empty())
}

/// Every text descendant, markup removed, whitespace collapsed, bounded.
///
/// Iterative, through `descendants`, so a deeply nested document cannot drive
/// this into the stack. HTML inside a CDATA section arrives as text and is
/// stripped of its tags here as well: what a client is handed is a plain
/// sentence, never markup a feed chose.
fn collect_text(node: Node<'_, '_>, limit: usize) -> String {
    let budget = limit.saturating_mul(8);
    let mut raw = String::new();
    for descendant in node.descendants() {
        if descendant.is_text() {
            if let Some(text) = descendant.text() {
                raw.push_str(markup::clip(text, budget));
                raw.push(' ');
                if raw.len() > budget {
                    break;
                }
            }
        }
    }
    markup::html_to_line(&raw, limit)
}

/// An attribute by its local name, whatever namespace it is in: RSS 1.0 spells
/// `enc:url` and `rdf:resource` where RSS 2.0 spells `url`.
fn attribute_named<'a>(node: Node<'a, '_>, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        node.attributes()
            .find(|attribute| attribute.name() == *name)
            .map(|attribute| attribute.value())
    })
}

/// An address out of the document, resolved against `base`, kept only when a
/// fetch may try it.
fn address(value: &str, base: &Url) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_URL_CHARS {
        return None;
    }
    let joined = base.join(value).ok()?;
    parse_address(joined.as_str()).map(String::from)
}

fn channel_image(node: Node<'_, '_>, base: &Url) -> Option<String> {
    node.children()
        .filter(|candidate| candidate.is_element() && candidate.tag_name().name() == "image")
        .find_map(|image| {
            image
                .attribute("href")
                .and_then(|href| address(href, base))
                .or_else(|| text_of(image, "url", MAX_URL_CHARS).and_then(|u| address(&u, base)))
        })
}

/// `itunes:author`, or Atom's `author` with its `name` inside.
fn author_of(node: Node<'_, '_>) -> Option<String> {
    let author = child(node, "author")?;
    child(author, "name")
        .map(|name| collect_text(name, MAX_TITLE_CHARS))
        .filter(|name| !name.is_empty())
        .or_else(|| text_of(node, "author", MAX_TITLE_CHARS))
}

fn parse_rss(channel: Node<'_, '_>, base: &Url) -> Feed {
    let episodes = channel
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .filter_map(|item| rss_item(item, base))
        .collect();
    Feed {
        title: text_of(channel, "title", MAX_TITLE_CHARS).unwrap_or_default(),
        description: text_of(channel, "description", MAX_DESCRIPTION_CHARS)
            .or_else(|| text_of(channel, "summary", MAX_DESCRIPTION_CHARS)),
        author: author_of(channel),
        language: text_of(channel, "language", 32),
        site_url: text_of(channel, "link", MAX_URL_CHARS).and_then(|link| address(&link, base)),
        image_url: channel_image(channel, base),
        episodes,
    }
}

/// An enclosure's declared type, bounded; `None` when it declares none.
fn declared_type(node: Node<'_, '_>) -> Option<String> {
    attribute_named(node, &["type"])
        .map(|kind| kind.trim().chars().take(100).collect::<String>())
        .filter(|kind| !kind.is_empty())
}

/// Whether a declared type rules audio out. Video is out of scope, and a page,
/// an image or a document is not an episode. A missing or odd type proves
/// nothing, since feeds get this wrong often: the bytes decide when a file is
/// opened (`sniff_audio`).
fn type_rules_out_audio(kind: Option<&str>) -> bool {
    kind.is_some_and(|kind| {
        let kind = kind.to_ascii_lowercase();
        ["video/", "image/", "text/"]
            .iter()
            .any(|prefix| kind.starts_with(prefix))
            || kind == "application/pdf"
    })
}

fn declared_length(node: Node<'_, '_>) -> Option<i64> {
    attribute_named(node, &["length"])
        .and_then(|length| length.trim().parse::<i64>().ok())
        .filter(|length| *length > 0)
}

fn rss_item(item: Node<'_, '_>, base: &Url) -> Option<FeedEpisode> {
    let (enclosure, enclosure_url) = item
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "enclosure")
        .filter(|node| !type_rules_out_audio(declared_type(*node).as_deref()))
        .find_map(|node| {
            let target = attribute_named(node, &["url", "resource"])?;
            Some((node, address(target, base)?))
        })?;
    let guid = text_of(item, "guid", MAX_URL_CHARS).unwrap_or_else(|| enclosure_url.clone());
    Some(episode(
        item,
        base,
        guid,
        text_of(item, "description", MAX_DESCRIPTION_CHARS)
            .or_else(|| text_of(item, "summary", MAX_DESCRIPTION_CHARS)),
        text_of(item, "pubDate", 100).or_else(|| text_of(item, "date", 100)),
        enclosure,
        enclosure_url,
    ))
}

/// What the three formats share once they have found an item's enclosure.
fn episode(
    item: Node<'_, '_>,
    base: &Url,
    guid: String,
    description: Option<String>,
    date: Option<String>,
    enclosure: Node<'_, '_>,
    enclosure_url: String,
) -> FeedEpisode {
    let (chapters_url, inline_chapters) = item_chapters(item, base);
    FeedEpisode {
        guid,
        title: text_of(item, "title", MAX_TITLE_CHARS).unwrap_or_default(),
        description,
        published_at: date.and_then(|date| parse_date(&date)),
        duration_ms: text_of(item, "duration", 32).and_then(|value| parse_duration(&value)),
        season: text_of(item, "season", 12).and_then(|value| parse_ordinal(&value)),
        episode_number: text_of(item, "episode", 12).and_then(|value| parse_ordinal(&value)),
        enclosure_type: declared_type(enclosure),
        enclosure_length: declared_length(enclosure),
        enclosure_url,
        chapters_url,
        inline_chapters,
        transcripts: item_transcripts(item, base),
    }
}

/// `podcast:chapters` names a file through `url`; `psc:chapters` holds its
/// chapters inside. Told apart by shape, since a feed may bind either namespace
/// to any prefix.
fn item_chapters(item: Node<'_, '_>, base: &Url) -> (Option<String>, Vec<Chapter>) {
    let mut url = None;
    let mut inline = Vec::new();
    for node in item
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "chapters")
    {
        if let Some(target) = node.attribute("url") {
            let is_json = declared_type(node).is_none_or(|kind| kind.contains("json"));
            if url.is_none() && is_json {
                url = address(target, base);
            }
        } else if inline.is_empty() {
            inline = chapters::read_inline(node);
        }
    }
    (url, inline)
}

fn item_transcripts(item: Node<'_, '_>, base: &Url) -> Vec<TranscriptRef> {
    item.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "transcript")
        .filter_map(|node| {
            let url = address(node.attribute("url")?, base)?;
            let kind = TranscriptKind::recognise(declared_type(node).as_deref(), &url)?;
            Some(TranscriptRef {
                url,
                kind,
                language: node
                    .attribute("language")
                    .map(|language| language.trim().chars().take(32).collect::<String>())
                    .filter(|language| !language.is_empty()),
            })
        })
        .take(MAX_TRANSCRIPTS_PER_EPISODE)
        .collect()
}

fn parse_rdf(root: Node<'_, '_>, base: &Url) -> Feed {
    let channel = child(root, "channel");
    let episodes = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .filter_map(|item| rss_item(item, base))
        .collect();
    Feed {
        title: channel
            .and_then(|channel| text_of(channel, "title", MAX_TITLE_CHARS))
            .unwrap_or_default(),
        description: channel
            .and_then(|channel| text_of(channel, "description", MAX_DESCRIPTION_CHARS)),
        author: channel.and_then(|channel| text_of(channel, "creator", MAX_TITLE_CHARS)),
        language: channel.and_then(|channel| text_of(channel, "language", 32)),
        site_url: channel
            .and_then(|channel| text_of(channel, "link", MAX_URL_CHARS))
            .and_then(|link| address(&link, base)),
        image_url: root
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "image")
            .find_map(|image| text_of(image, "url", MAX_URL_CHARS).and_then(|u| address(&u, base))),
        episodes,
    }
}

/// The `href` of the first `link` with this `rel` (Atom's default is
/// `alternate`).
fn atom_links<'a, 'input>(
    node: Node<'a, 'input>,
    rel: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a
where
    'input: 'a,
{
    node.children().filter(move |candidate| {
        candidate.is_element()
            && candidate.tag_name().name() == "link"
            && candidate.attribute("rel").unwrap_or("alternate") == rel
            && candidate.attribute("href").is_some()
    })
}

fn atom_entry(entry: Node<'_, '_>, base: &Url) -> Option<FeedEpisode> {
    let (enclosure, enclosure_url) = atom_links(entry, "enclosure")
        .filter(|node| !type_rules_out_audio(declared_type(*node).as_deref()))
        .find_map(|node| Some((node, address(node.attribute("href")?, base)?)))?;
    Some(episode(
        entry,
        base,
        text_of(entry, "id", MAX_URL_CHARS).unwrap_or_else(|| enclosure_url.clone()),
        text_of(entry, "summary", MAX_DESCRIPTION_CHARS)
            .or_else(|| text_of(entry, "content", MAX_DESCRIPTION_CHARS)),
        text_of(entry, "published", 100).or_else(|| text_of(entry, "updated", 100)),
        enclosure,
        enclosure_url,
    ))
}

fn parse_atom(root: Node<'_, '_>, base: &Url) -> Feed {
    let episodes = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "entry")
        .filter_map(|entry| atom_entry(entry, base))
        .collect();
    Feed {
        title: text_of(root, "title", MAX_TITLE_CHARS).unwrap_or_default(),
        description: text_of(root, "subtitle", MAX_DESCRIPTION_CHARS),
        author: author_of(root),
        language: root
            .attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
            .map(|language| language.trim().chars().take(32).collect::<String>())
            .filter(|language| !language.is_empty()),
        site_url: atom_links(root, "alternate")
            .next()
            .and_then(|link| link.attribute("href"))
            .and_then(|href| address(href, base)),
        image_url: text_of(root, "logo", MAX_URL_CHARS)
            .or_else(|| text_of(root, "icon", MAX_URL_CHARS))
            .and_then(|logo| address(&logo, base))
            .or_else(|| channel_image(root, base)),
        episodes,
    }
}

/// RFC 2822 (RSS), then RFC 3339 (Atom), in epoch milliseconds. An impossible
/// date, or one no podcast was or will be published on, is no date.
pub(super) fn parse_date(value: &str) -> Option<i64> {
    let value = value.trim();
    chrono::DateTime::parse_from_rfc2822(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(value))
        .ok()
        .map(|date| date.timestamp_millis())
        .filter(|ms| (EARLIEST_YEAR_MS..LATEST_YEAR_MS).contains(ms))
}

/// `itunes:duration`: seconds, `MM:SS` or `HH:MM:SS`. Zero is what a feed says
/// when it does not know.
pub(super) fn parse_duration(value: &str) -> Option<i64> {
    let mut seconds: i64 = 0;
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for part in parts {
        let number: f64 = part.trim().parse().ok()?;
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        seconds = seconds.checked_mul(60)?.checked_add(number as i64)?;
    }
    seconds
        .checked_mul(1000)
        .filter(|ms| *ms > 0 && (*ms as u64) < MAX_PLAUSIBLE_MS)
}

/// `itunes:season` and `itunes:episode`: a small positive number.
fn parse_ordinal(value: &str) -> Option<i64> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|number| (1..1_000_000).contains(number))
}
