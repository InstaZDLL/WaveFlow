//! Decode a text file the user brought, whatever encoding it was saved in.
//!
//! Sidecar lyrics, an `.lrc` picked for import and an `.m3u` playlist come
//! from other tools, and many of those write the system's code page rather
//! than UTF-8: GB18030 on a Chinese Windows, Big5, Shift_JIS, EUC-KR, or
//! Windows-1252 for an old Western file. Read as UTF-8, such a file failed
//! outright, and the lyrics it held were simply not there (#910).
//!
//! A byte-order mark names the encoding (UTF-8, UTF-16 LE or BE) and is
//! dropped. Otherwise valid UTF-8 is UTF-8, which is what nearly every file
//! is, at no copy. Only what is neither has its encoding guessed from the
//! bytes. A fixed fallback (always GB18030, say) would not do: GB18030
//! accepts almost any byte sequence, so a Japanese or Korean file would
//! come back as Chinese-looking nonsense instead of its text.

use std::borrow::Cow;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};

/// The text of `bytes`, decoded as described in the module docs. Never
/// fails: a byte no encoding accounts for becomes U+FFFD.
pub fn decode(bytes: &[u8]) -> Cow<'_, str> {
    if let Some((encoding, bom_length)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode_without_bom_handling(&bytes[bom_length..]).0;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(text);
    }
    // ISO-2022-JP is 7-bit, so valid UTF-8 and already taken above.
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    // Not UTF-8: that was ruled out by the check above.
    let encoding = detector.guess(None, Utf8Detection::Deny);
    encoding.decode_without_bom_handling(bytes).0
}

#[cfg(test)]
mod tests;
