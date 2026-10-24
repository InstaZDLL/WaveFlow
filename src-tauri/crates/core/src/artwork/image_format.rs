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
mod tests {
    use super::detect_image_format;

    #[test]
    fn names_the_three_stored_formats() {
        assert_eq!(detect_image_format(b"\xFF\xD8\xFF\xE0rest"), Some("jpg"));
        assert_eq!(
            detect_image_format(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some("png")
        );
        assert_eq!(
            detect_image_format(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some("webp")
        );
    }

    #[test]
    fn refuses_anything_else() {
        assert_eq!(detect_image_format(b""), None);
        assert_eq!(detect_image_format(b"\xFF\xD8"), None);
        // A PNG signature cut short is not a PNG.
        assert_eq!(detect_image_format(b"\x89PNG"), None);
        assert_eq!(detect_image_format(b"RIFF\x24\x00\x00\x00WAVE"), None);
        assert_eq!(detect_image_format(b"GIF89a"), None);
        assert_eq!(detect_image_format(b"not an image"), None);
    }
}
