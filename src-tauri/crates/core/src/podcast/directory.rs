//! The iTunes podcast directory: the addresses that ask it something, and what
//! its answers become.
//!
//! Three kinds of answer are read, all JSON from a third party, so all hostile:
//!
//! - **a search or a lookup** (`itunes.apple.com/search`, `/lookup`), whose
//!   `results` carry each show's feed address;
//! - **a country's chart** (`itunes.apple.com/{cc}/rss/toppodcasts`), whose
//!   `feed.entry` carries no feed address, so subscribing to one of its shows
//!   asks a lookup first. It is the only chart that can be narrowed to a genre.
//!   A chart holding one entry has it as an object rather than a list.
//!
//! Every answer is bounded before it is parsed ([`MAX_DIRECTORY_BYTES`]), and
//! `serde_json` refuses a nesting deeper than 128 before it can exhaust a
//! stack. What is kept of a show is bounded again: an id that is a positive
//! integer, a title and an author as one line of plain text, a feed address
//! that [`parse_address`] accepts, a picture over https only, at most
//! [`MAX_DIRECTORY_RESULTS`] shows, each once. A show without an id or a title
//! is not a show. A field of the wrong type is treated as absent rather than
//! refusing the whole answer: the directory changes its shapes now and then,
//! and one odd show must not hide the others.
//!
//! Nothing here logs, and nothing here knows which network an address may
//! reach: the app decides that, and a feed found here is held to public
//! addresses, as one out of an OPML list is.

use serde_json::Value;
use url::Url;

use super::address::parse_address;

/// The largest answer read, in bytes. A chart of 200 shows weighs about 500 KB
/// and a search of 200 about 400 KB.
pub const MAX_DIRECTORY_BYTES: usize = 4 * 1024 * 1024;

/// The most shows one answer gives, and the most a chart or a search asks for.
pub const MAX_DIRECTORY_RESULTS: usize = 200;

/// The most ids one lookup asks about: a whole chart at once.
pub const MAX_LOOKUP_IDS: usize = MAX_DIRECTORY_RESULTS;

/// The longest search term sent, in characters.
pub const MAX_TERM_CHARS: usize = 100;

/// The longest title or author kept, in characters.
const MAX_TEXT_CHARS: usize = 300;

/// The side, in pixels, of the picture asked for a show.
pub const THUMBNAIL_SIDE: u32 = 300;

/// The largest id believed. Real ones have ten digits; this keeps one that
/// would not survive a trip through a JavaScript number from being kept.
const MAX_SHOW_ID: u64 = (1 << 53) - 1;

/// The directory's top-level podcast genres, by the id it gives them. Their
/// names are the app's own, translated with the rest of the interface, so a
/// genre reads in the interface's language whatever the country browsed.
///
/// Each was checked against the chart it narrows to (2026-11-25): an id the
/// directory no longer knows, such as the 1307 and 1325 of its older list,
/// answers the whole chart instead, without saying so.
pub const GENRES: &[u32] = &[
    1301, // Arts
    1303, // Comedy
    1304, // Education
    1305, // Kids & Family
    1309, // TV & Film
    1310, // Music
    1314, // Religion & Spirituality
    1318, // Technology
    1321, // Business
    1324, // Society & Culture
    1511, // Government
    1512, // Health & Fitness
    1483, // Fiction
    1487, // History
    1488, // True Crime
    1489, // News
    1502, // Leisure
    1533, // Science
    1545, // Sports
];

/// One show as the directory lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryShow {
    /// The directory's id for it, the one a lookup takes.
    pub id: u64,
    pub title: String,
    pub author: Option<String>,
    /// Its first genre the directory gives that is one of [`GENRES`]. A chart
    /// names a sub-genre (Books rather than Arts), so its shows have none here.
    pub genre_id: Option<u32>,
    /// Its picture, over https, asked at [`THUMBNAIL_SIDE`] when the address
    /// says how.
    pub artwork_url: Option<String>,
    /// Its feed. A chart gives none: a lookup does.
    pub feed_url: Option<String>,
    pub episode_count: Option<u32>,
    pub explicit: bool,
}

/// Why an answer gave nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryError {
    /// Larger than [`MAX_DIRECTORY_BYTES`].
    TooLarge,
    /// Not JSON, nested past the parser's limit, or not the shape asked for.
    Invalid,
}

/// A country the directory may be asked about: two ASCII letters, as the
/// directory writes them (lower case). Which storefronts exist is the
/// interface's list; an unknown one only answers an empty chart.
pub fn country_code(raw: &str) -> Option<String> {
    let raw = raw.trim();
    (raw.len() == 2 && raw.bytes().all(|byte| byte.is_ascii_alphabetic()))
        .then(|| raw.to_ascii_lowercase())
}

/// A term as it is sent: one line, at most [`MAX_TERM_CHARS`] characters.
/// `None` when nothing is left to search for.
pub fn search_term(raw: &str) -> Option<String> {
    let term = crate::markup::collapse(raw, MAX_TERM_CHARS);
    (!term.is_empty()).then_some(term)
}

/// The address of a search for `term` in `country`'s storefront.
pub fn search_url(term: &str, country: &str) -> Url {
    let mut url = Url::parse("https://itunes.apple.com/search").expect("a constant address");
    url.query_pairs_mut()
        .append_pair("media", "podcast")
        .append_pair("entity", "podcast")
        .append_pair("term", term)
        .append_pair("country", country)
        .append_pair("limit", &MAX_DIRECTORY_RESULTS.to_string());
    url
}

/// The address of `country`'s chart, narrowed to `genre` when there is one.
pub fn chart_url(country: &str, genre: Option<u32>) -> Url {
    let genre = genre
        .filter(|genre| GENRES.contains(genre))
        .map(|genre| format!("/genre={genre}"))
        .unwrap_or_default();
    Url::parse(&format!(
        "https://itunes.apple.com/{country}/rss/toppodcasts/limit={MAX_DIRECTORY_RESULTS}{genre}/json"
    ))
    .expect("a country code is two letters")
}

/// The address of a lookup of `ids` in `country`'s storefront: every show of a
/// chart in one request.
pub fn lookup_url(ids: &[u64], country: &str) -> Url {
    let ids = ids
        .iter()
        .take(MAX_LOOKUP_IDS)
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut url = Url::parse("https://itunes.apple.com/lookup").expect("a constant address");
    url.query_pairs_mut()
        .append_pair("id", &ids)
        .append_pair("entity", "podcast")
        .append_pair("country", country);
    url
}

/// The shows of a search or a lookup's answer.
pub fn parse_search(bytes: &[u8]) -> Result<Vec<DirectoryShow>, DirectoryError> {
    let root = parse_json(bytes)?;
    let results = root
        .get("results")
        .and_then(Value::as_array)
        .ok_or(DirectoryError::Invalid)?;
    Ok(collect(results.iter().filter_map(search_show)))
}

/// The shows of a chart's answer, in their order on it.
pub fn parse_chart(bytes: &[u8]) -> Result<Vec<DirectoryShow>, DirectoryError> {
    let root = parse_json(bytes)?;
    let feed = root
        .get("feed")
        .filter(|feed| feed.is_object())
        .ok_or(DirectoryError::Invalid)?;
    let entries: Vec<&Value> = match feed.get("entry") {
        // An empty chart has no entry at all.
        None => Vec::new(),
        Some(Value::Array(entries)) => entries.iter().collect(),
        Some(entry @ Value::Object(_)) => vec![entry],
        Some(_) => return Err(DirectoryError::Invalid),
    };
    Ok(collect(entries.into_iter().filter_map(chart_show)))
}

/// The address of a picture as it is kept: https only, and asked at
/// [`THUMBNAIL_SIDE`] when it is one of the directory's resizable ones
/// (`…/100x100bb.jpg`).
pub fn thumbnail_url(raw: &str) -> Option<String> {
    let mut url = parse_address(raw)?;
    if url.scheme() != "https" {
        return None;
    }
    let resizable = url
        .host_str()
        .is_some_and(|host| host.ends_with(".mzstatic.com"));
    if resizable {
        let path = url.path().to_owned();
        if let Some((dir, last)) = path.rsplit_once('/') {
            if let Some(extension) = sized_name_extension(last) {
                url.set_path(&format!(
                    "{dir}/{THUMBNAIL_SIDE}x{THUMBNAIL_SIDE}bb.{extension}"
                ));
            }
        }
    }
    Some(url.to_string())
}

/// The extension of a name such as `100x100bb.jpg`, when it is one.
fn sized_name_extension(name: &str) -> Option<&str> {
    let (size, extension) = name.split_once("bb.")?;
    let (width, height) = size.split_once('x')?;
    let digits =
        |part: &str| (1..=4).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
    let extension_ok =
        (1..=4).contains(&extension.len()) && extension.bytes().all(|b| b.is_ascii_alphanumeric());
    (digits(width) && digits(height) && extension_ok).then_some(extension)
}

fn parse_json(bytes: &[u8]) -> Result<Value, DirectoryError> {
    if bytes.len() > MAX_DIRECTORY_BYTES {
        return Err(DirectoryError::TooLarge);
    }
    let root: Value = serde_json::from_slice(bytes).map_err(|_| DirectoryError::Invalid)?;
    root.is_object()
        .then_some(root)
        .ok_or(DirectoryError::Invalid)
}

/// Each show once, the first time it is met, and at most
/// [`MAX_DIRECTORY_RESULTS`] of them.
fn collect(shows: impl Iterator<Item = DirectoryShow>) -> Vec<DirectoryShow> {
    let mut seen = std::collections::HashSet::new();
    shows
        .filter(|show| seen.insert(show.id))
        .take(MAX_DIRECTORY_RESULTS)
        .collect()
}

fn search_show(value: &Value) -> Option<DirectoryShow> {
    if value
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "podcast")
    {
        return None;
    }
    let id = value.get("collectionId").and_then(id_of)?;
    let title = value
        .get("collectionName")
        .and_then(line)
        .or_else(|| value.get("trackName").and_then(line))?;
    let genre_id = value
        .get("genreIds")
        .and_then(Value::as_array)
        .and_then(|ids| ids.iter().filter_map(genre_of).next());
    let artwork_url = ["artworkUrl100", "artworkUrl600", "artworkUrl60"]
        .iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(Value::as_str)
                .and_then(thumbnail_url)
        });
    let explicit = value
        .get("collectionExplicitness")
        .and_then(Value::as_str)
        .is_some_and(|word| word == "explicit");
    Some(DirectoryShow {
        id,
        title,
        author: value.get("artistName").and_then(line),
        genre_id,
        artwork_url,
        feed_url: value
            .get("feedUrl")
            .and_then(Value::as_str)
            .and_then(parse_address)
            .map(String::from),
        episode_count: value
            .get("trackCount")
            .and_then(Value::as_u64)
            .and_then(|count| u32::try_from(count).ok()),
        explicit,
    })
}

fn chart_show(entry: &Value) -> Option<DirectoryShow> {
    let id = entry
        .get("id")
        .and_then(|id| id.get("attributes"))
        .and_then(|attributes| attributes.get("im:id"))
        .and_then(id_of)?;
    let title = label(entry.get("im:name"))?;
    let genre_id = entry
        .get("category")
        .and_then(|category| category.get("attributes"))
        .and_then(|attributes| attributes.get("im:id"))
        .and_then(genre_of);
    // The pictures come smallest first, each with its height.
    let artwork_url = entry
        .get("im:image")
        .and_then(Value::as_array)
        .and_then(|images| {
            images.iter().rev().find_map(|image| {
                image
                    .get("label")
                    .and_then(Value::as_str)
                    .and_then(thumbnail_url)
            })
        });
    Some(DirectoryShow {
        id,
        title,
        author: label(entry.get("im:artist")),
        genre_id,
        artwork_url,
        feed_url: None,
        episode_count: None,
        explicit: false,
    })
}

/// `{"label": "…"}`, as one line of plain text.
fn label(value: Option<&Value>) -> Option<String> {
    value?.get("label").and_then(line)
}

/// A string as one line of plain text, `None` when nothing is left.
fn line(value: &Value) -> Option<String> {
    let text = crate::markup::html_to_line(value.as_str()?, MAX_TEXT_CHARS);
    (!text.is_empty()).then_some(text)
}

/// A positive integer id, written as a number or as a string of digits.
fn id_of(value: &Value) -> Option<u64> {
    let id = match value {
        Value::Number(number) => number.as_u64()?,
        Value::String(text)
            if (1..=16).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit()) =>
        {
            text.parse().ok()?
        }
        _ => return None,
    };
    (1..=MAX_SHOW_ID).contains(&id).then_some(id)
}

/// A genre this module knows, written as a number or a string.
fn genre_of(value: &Value) -> Option<u32> {
    let id = u32::try_from(id_of(value)?).ok()?;
    GENRES.contains(&id).then_some(id)
}
