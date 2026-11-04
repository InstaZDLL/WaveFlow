//! Markup someone else wrote, turned into text.
//!
//! A podcast feed's description is HTML more often than not, a transcript
//! may be a whole web page, and a caption cue carries its own small tags.
//! None of it reaches the webview as markup: what the frontend is handed is
//! a plain sentence, whatever the document chose to put in it.
//!
//! The scan is linear in the input on purpose. These strings come from a
//! hostile document, up to several megabytes of it, and a stripper that
//! looks ahead for a closing `>` from every `<` is quadratic on a string of
//! nothing but `<a `.

use std::borrow::Cow;

/// The tags that separate text rather than decorate it. `<b>bo</b>ld` is one
/// word, and `<p>one</p><p>two</p>` is two lines.
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

/// HTML with its tags, comments, scripts and style sheets removed, a block
/// tag left as a line break and entities decoded once.
///
/// `<` starts a tag only where a tag could start, so `I <3 you` and `a < b`
/// keep their text. A doubly escaped `&lt;b&gt;` comes out as the literal
/// `<b>`, which is what the author wrote and is only text here.
pub fn html_to_text(raw: &str) -> String {
    decode_entities(&strip(raw, false)).into_owned()
}

/// A caption cue's payload: every `<…>` goes, including the `<00:00:01.000>`
/// and `<c.yellow>` a cue may carry, and the entities a cue may use are
/// decoded once.
pub fn cue_to_text(raw: &str) -> String {
    decode_entities(&strip(raw, true)).into_owned()
}

/// HTML as one bounded line: tags removed, whitespace collapsed, control
/// characters dropped, at most `limit` characters.
///
/// What is read is bounded first, by the `limit` text is at worst eight bytes
/// a character of, so a single text node of 16 MiB costs what its bound says.
pub fn html_to_line(raw: &str, limit: usize) -> String {
    collapse(&html_to_text(clip(raw, limit.saturating_mul(8))), limit)
}

/// Whitespace collapsed to single spaces, control characters dropped, trimmed,
/// at most `limit` characters.
pub fn collapse(raw: &str, limit: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(limit.saturating_mul(4)));
    let mut pushed = 0usize;
    let mut last_space = true;
    for character in raw.chars() {
        if pushed >= limit {
            break;
        }
        if character.is_whitespace() {
            if !last_space {
                out.push(' ');
                pushed += 1;
                last_space = true;
            }
        } else if !character.is_control() {
            out.push(character);
            pushed += 1;
            last_space = false;
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// At most `bytes` bytes of `raw`, cut on a character boundary.
pub fn clip(raw: &str, bytes: usize) -> &str {
    if raw.len() <= bytes {
        return raw;
    }
    let mut end = bytes;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    &raw[..end]
}

fn strip(raw: &str, every_angle: bool) -> String {
    let mut out = String::with_capacity(raw.len().min(64 * 1024));
    let mut rest = raw;
    // Once one `find('>')` has come back empty no later one can succeed, and
    // remembering it is what keeps a string of `<a <a <a` linear.
    let mut no_close_left = false;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => {
                    rest = &rest[end + 3..];
                    continue;
                }
                None => return out,
            }
        }
        let could_be_tag = every_angle
            || rest[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
        let close = if could_be_tag && !no_close_left {
            rest.find('>')
        } else {
            None
        };
        let Some(close) = close else {
            no_close_left = no_close_left || could_be_tag;
            out.push('<');
            rest = &rest[1..];
            continue;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        let name = tag_name(tag);
        if !tag.starts_with('/')
            && !tag.ends_with('/')
            && (name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style"))
        {
            // The content of these is not text. Their closing tag is left for
            // the loop to eat, and a missing one takes the rest of the input.
            match find_closing(rest, name) {
                Some(at) => {
                    rest = &rest[at..];
                    continue;
                }
                None => return out,
            }
        }
        if BLOCK_TAGS.iter().any(|b| name.eq_ignore_ascii_case(b)) {
            out.push('\n');
        }
    }
    out.push_str(rest);
    out
}

/// The name of a tag, from what is between its angle brackets.
fn tag_name(tag: &str) -> &str {
    let tag = tag.strip_prefix('/').unwrap_or(tag);
    let end = tag
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(tag.len());
    &tag[..end]
}

/// Where `</name` starts in `rest`, ignoring case.
fn find_closing(rest: &str, name: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let needle = name.as_bytes();
    let mut from = 0;
    while let Some(at) = rest[from..].find("</") {
        let start = from + at;
        let name_at = start + 2;
        if bytes.len() >= name_at + needle.len()
            && bytes[name_at..name_at + needle.len()].eq_ignore_ascii_case(needle)
        {
            return Some(start);
        }
        from = name_at;
    }
    None
}

/// Named and numeric character references decoded, once. One that is unknown
/// or names no character stays as written.
pub fn decode_entities(raw: &str) -> Cow<'_, str> {
    if !raw.contains('&') {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // The longest name below is eight characters; a reference is short,
        // and looking no further keeps a string of `&&&&` linear.
        let window = clip(rest, 12);
        let decoded = window[1..].find(';').and_then(|semi| {
            let reference = &window[1..semi + 1];
            decode_reference(reference).map(|c| (c, semi + 2))
        });
        match decoded {
            Some((character, used)) => {
                out.push(character);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn decode_reference(reference: &str) -> Option<char> {
    if let Some(number) = reference.strip_prefix('#') {
        let value = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(value).filter(|c| *c != '\0');
    }
    Some(match reference {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "agrave" => 'à',
        "aacute" => 'á',
        "acirc" => 'â',
        "auml" => 'ä',
        "ccedil" => 'ç',
        "egrave" => 'è',
        "eacute" => 'é',
        "ecirc" => 'ê',
        "euml" => 'ë',
        "iacute" => 'í',
        "icirc" => 'î',
        "iuml" => 'ï',
        "ntilde" => 'ñ',
        "oacute" => 'ó',
        "ocirc" => 'ô',
        "ouml" => 'ö',
        "ugrave" => 'ù',
        "uacute" => 'ú',
        "ucirc" => 'û',
        "uuml" => 'ü',
        "szlig" => 'ß',
        "Agrave" => 'À',
        "Eacute" => 'É',
        "Egrave" => 'È',
        "Ccedil" => 'Ç',
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
