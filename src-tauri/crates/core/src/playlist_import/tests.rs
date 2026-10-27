use super::*;

#[test]
fn an_isrc_is_compared_without_its_hyphens_and_case() {
    assert_eq!(
        normalize_isrc("gb-n9y-11-00088").as_deref(),
        Some("GBN9Y1100088")
    );
    assert_eq!(
        normalize_isrc(" USRC17607839 ").as_deref(),
        Some("USRC17607839")
    );
    assert_eq!(normalize_isrc("not an isrc"), None);
    assert_eq!(normalize_isrc("USRC1760783"), None);
    assert_eq!(normalize_isrc(""), None);
}

#[test]
fn a_path_key_is_built_without_touching_the_disk() {
    use std::path::Path;
    let sep = std::path::MAIN_SEPARATOR;
    let key = path_key(Path::new("/music/./a/../b/song.flac"));
    assert_eq!(key, format!("{sep}music{sep}b{sep}song.flac"));
    if cfg!(windows) {
        assert_eq!(
            path_key(Path::new(r"\\?\C:\Music\Song.FLAC")),
            path_key(Path::new("c:/music/song.flac"))
        );
        assert_eq!(
            path_key(Path::new(r"\\?\UNC\server\share\x.mp3")),
            r"\\server\share\x.mp3"
        );
        // A share another machine names is compared, never opened.
        assert_eq!(
            path_key(Path::new(r"\\203.0.113.9\music\x.mp3")),
            r"\\203.0.113.9\music\x.mp3"
        );
    }
}

#[test]
fn a_file_name_is_found_whatever_the_separator() {
    assert_eq!(file_name(r"C:\Music\a.flac"), Some("a.flac"));
    assert_eq!(file_name("/music/b.mp3"), Some("b.mp3"));
    assert_eq!(file_name("c.ogg"), Some("c.ogg"));
    assert_eq!(file_name("/music/"), None);
}

#[test]
fn durations_are_read_the_way_exports_write_them() {
    assert_eq!(parse_duration("3:45", false), Some(225_000));
    assert_eq!(parse_duration("1:02:03", false), Some(3_723_000));
    assert_eq!(parse_duration("225", false), Some(225_000));
    assert_eq!(parse_duration("225.4", false), Some(225_400));
    assert_eq!(parse_duration("225000", true), Some(225_000));
    // Milliseconds in a column that does not say so.
    assert_eq!(parse_duration("225000", false), Some(225_000));
    assert_eq!(parse_duration("", false), None);
    assert_eq!(parse_duration("-1", false), None);
    assert_eq!(parse_duration("abc", false), None);
    assert_eq!(parse_duration("3:75", false), None);
    // Absurd, and refused rather than overflowed.
    assert_eq!(parse_duration("200000000000000:00", false), None);
}
