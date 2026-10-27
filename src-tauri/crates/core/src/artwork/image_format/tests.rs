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
