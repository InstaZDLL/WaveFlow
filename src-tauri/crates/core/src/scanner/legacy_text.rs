//! Tags written in a legacy CJK code page, which lofty reads as Latin-1.
//!
//! An ID3v1 tag has no way to name its encoding, and plenty of ID3v2.3 files
//! declare Latin-1 (`0`) while holding what the system's code page produced:
//! GBK on a Chinese Windows, Big5, Shift_JIS, EUC-KR. lofty takes the
//! declaration at its word, so 夜曲 (`D2 B9 C7 FA` in GBK) reaches the library
//! as `Ò¹Çú`.
//!
//! Latin-1 maps every byte to the code point of the same value, so what lofty
//! returns converts back to the exact bytes, losslessly. [`repair_tag`] does
//! that and decodes them under the encoding the whole tag points to:
//!
//! - **The guess is over every text field of the tag**, not one field: a
//!   title of two characters is too little to tell GBK from Big5, a title
//!   with the artist and the album is not.
//! - **Only a multi-byte CJK encoding is ever accepted, and only when every
//!   field decodes without a replacement character and into characters a CJK
//!   title is made of.** Anything else, and "Café", "Ærøskøbing" and "Señor"
//!   with it, is left exactly as lofty read it: Latin-1 is the one encoding
//!   nothing can prove wrong, so a Western tag is never second-guessed
//!   beyond the three tests below.
//! - **A tag has to look like a code page, not like a Western word.** A
//!   Latin-1 reading of GBK or Shift_JIS shows two things a Western text does
//!   not: runs of two or more accented letters in a row, and C1 control
//!   characters (`U+0080`–`U+009F`, which are lead bytes of Shift_JIS). And a
//!   lowercase accented letter between two ASCII letters (`ñ` in "Señor") is
//!   the signature of a Western word, which vetoes the whole tag.
//!
//! The same reasoning applies to every reader: the repair runs inside
//! [`read_tagged_file`](super::extract::read_tagged_file) and
//! [`read_tagged_from`](super::extract::read_tagged_from), so the scanner,
//! a file played without importing it, a network share and the tag editor
//! all see the same text, and the multi-artist split (`"; "`) sees the
//! repaired credit.

pub mod utf8_frames;

use encoding_rs::Encoding;
use lofty::file::TaggedFile;
use lofty::tag::{ItemValue, Tag, TagItem, TagType};

/// The encodings a legacy tag can be in. Not UTF-16 (it has a byte-order mark
/// and ID3 declares it), not Windows-1252 (it is what lofty already shows).
const CANDIDATES: [&Encoding; 4] = [
    encoding_rs::GBK,
    encoding_rs::BIG5,
    encoding_rs::SHIFT_JIS,
    encoding_rs::EUC_KR,
];

/// Whether `text` could be a legacy code page read as Latin-1: Latin-1
/// characters only, at least one of them above ASCII. A pass over a library
/// that is already stored uses it to pick the rows worth reading again.
pub fn is_candidate(text: &str) -> bool {
    latin1_bytes(text).is_some_and(|bytes| !bytes.is_ascii())
}

/// Repair every ID3 tag of a file in place. Other containers declare UTF-8
/// and lofty enforces it, so there is nothing of this kind to recover there.
pub fn repair_tagged_file(tagged: &mut TaggedFile) {
    use lofty::file::TaggedFileExt;
    // `tags()` is read-only; `primary_tag_mut` and `tag_mut` are how a tag is
    // reached for writing.
    let kinds: Vec<TagType> = tagged.tags().iter().map(Tag::tag_type).collect();
    for kind in kinds {
        if matches!(kind, TagType::Id3v1 | TagType::Id3v2) {
            if let Some(tag) = tagged.tag_mut(kind) {
                repair_tag(tag);
            }
        }
    }
}

/// Repair one tag in place; a tag that does not need it is not touched.
pub fn repair_tag(tag: &mut Tag) {
    let mut fields: Vec<Vec<u8>> = Vec::new();
    for item in tag.items() {
        if let ItemValue::Text(text) = item.value() {
            if let Some(bytes) = latin1_bytes(text) {
                if !bytes.is_ascii() {
                    fields.push(bytes);
                }
            }
        }
    }
    let Some(encoding) = recover(&fields) else {
        return;
    };
    // The items are put back in their order: a multi-value key keeps it.
    let items: Vec<TagItem> = tag.items().cloned().collect();
    tag.retain(|_| false);
    for item in items {
        let repaired = match item.value() {
            ItemValue::Text(text) => latin1_bytes(text)
                .filter(|bytes| !bytes.is_ascii())
                .and_then(|bytes| decode_clean(encoding, &bytes)),
            _ => None,
        };
        match repaired {
            Some(text) => {
                // The language and the description are part of what a comment or
                // a lyrics item is: only its text was wrong.
                let mut repaired = TagItem::new(item.key(), ItemValue::Text(text));
                repaired.set_lang(*item.lang());
                repaired.set_description(item.description().to_owned());
                tag.push_unchecked(repaired);
            }
            None => tag.push_unchecked(item),
        }
    }
}

/// The encoding `fields` (non-ASCII, Latin-1 reading of the file's bytes) are
/// in, if they are in a legacy CJK one: see the module docs for the tests.
pub fn recover(fields: &[Vec<u8>]) -> Option<&'static Encoding> {
    if fields.is_empty() {
        return None;
    }
    let mut c1 = false;
    let mut pairs = 0usize;
    for bytes in fields {
        if western_word(bytes) {
            return None;
        }
        c1 |= bytes.iter().any(|b| (0x80..=0x9f).contains(b));
        pairs += high_pairs(bytes);
    }
    if !c1 && pairs < 2 {
        return None;
    }

    let accepted: Vec<&'static Encoding> = CANDIDATES
        .iter()
        .copied()
        .filter(|encoding| {
            fields
                .iter()
                .all(|bytes| decode_clean(encoding, bytes).is_some_and(|text| cjk_text(&text)))
        })
        .collect();
    match accepted.as_slice() {
        [] => None,
        [only] => Some(only),
        several => {
            // More than one reads cleanly (GBK and Big5 accept each other's
            // bytes): the detector picks among them, over all fields at once.
            let joined: Vec<u8> = fields.join(&b' ');
            let guess = crate::text_encoding::guess(&joined);
            several.iter().copied().find(|e| *e == guess)
        }
    }
}

/// The bytes a Latin-1 reading stands for, or `None` for a character above
/// `U+00FF`: text that did not come from a byte-per-character decode.
fn latin1_bytes(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect()
}

fn decode_clean(encoding: &'static Encoding, bytes: &[u8]) -> Option<String> {
    encoding
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(|text| text.into_owned())
}

/// Characters a title in a CJK code page is made of: ASCII, ideographs, kana,
/// hangul, CJK and full-width punctuation. A private-use character or an
/// accented Latin letter in the decoded text means the encoding was wrong.
fn cjk_text(text: &str) -> bool {
    text.chars().all(|c| {
        c.is_ascii()
            || matches!(u32::from(c),
                0x2010..=0x203B      // general punctuation a CJK title uses
                | 0x3000..=0x30FF    // CJK punctuation, hiragana, katakana
                | 0x3130..=0x318F    // hangul compatibility jamo (ㅋㅋ)
                | 0x31F0..=0x31FF
                | 0x3400..=0x4DBF    // ideographs, extension A
                | 0x4E00..=0x9FFF
                | 0xAC00..=0xD7A3    // hangul syllables
                | 0xF900..=0xFAFF    // compatibility ideographs
                | 0xFF00..=0xFFEF    // full-width forms
                | 0x20000..=0x2FFFF) // ideographs, extensions B and up
    })
}

/// A lowercase accented letter between two ASCII letters: "Señor", "Café s".
fn western_word(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|w| {
        w[0].is_ascii_alphabetic() && (0xdf..=0xff).contains(&w[1]) && w[2].is_ascii_alphabetic()
    })
}

/// Adjacent pairs of bytes that are both above `0x80`, counted without
/// overlap: GBK, Big5 and EUC-KR write their characters like that.
fn high_pairs(bytes: &[u8]) -> usize {
    let mut pairs = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] >= 0x80 && bytes[i + 1] >= 0x80 {
            pairs += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    pairs
}

#[cfg(test)]
mod tests;
