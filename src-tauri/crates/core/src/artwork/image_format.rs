//! Magic-byte sniffing for the image formats WaveFlow stores as covers.
//!
//! "Check the magic bytes, don't fully parse": enough to name a file's
//! extension, refuse something that is not an image at all, or type a cover
//! whose name says nothing (`.bin`). Pure, so the uploads, the metadata
//! downloads and the DLNA server all share one answer.

/// The extension for `bytes` — `"jpg"`, `"png"` or `"webp"` — or `None` for
/// anything else.
pub fn detect_image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("jpg");
    }
    if bytes.len() >= 8
        && bytes[0] == 0x89
        && bytes[1] == 0x50
        && bytes[2] == 0x4E
        && bytes[3] == 0x47
        && bytes[4] == 0x0D
        && bytes[5] == 0x0A
        && bytes[6] == 0x1A
        && bytes[7] == 0x0A
    {
        return Some("png");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

#[cfg(test)]
mod tests;
