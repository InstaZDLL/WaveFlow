use super::*;

fn urls(entries: &[PlaylistEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.url.as_str()).collect()
}

#[test]
fn an_extended_m3u_gives_its_streams_and_their_titles() {
    let text = "#EXTM3U\r\n#EXTINF:-1,Radio One, the first\r\nhttp://a.example/live\r\n\r\n# a comment\r\nhttps://b.example/mp3\r\n";
    let entries = parse_playlist(text).unwrap();
    assert_eq!(
        urls(&entries),
        ["http://a.example/live", "https://b.example/mp3"]
    );
    assert_eq!(entries[0].title.as_deref(), Some("Radio One, the first"));
    assert_eq!(
        entries[1].title, None,
        "a title belongs to the next line only"
    );
}

/// A comment in another script: the prefix test must not cut a
/// character in half.
#[test]
fn a_non_ascii_line_is_read_not_split() {
    let text = "#EXTM3U\n# Радио Рекорд\n# 日本のラジオ\nhttp://a.example/live\n";
    let entries = parse_playlist(text).unwrap();
    assert_eq!(urls(&entries), ["http://a.example/live"]);
}

#[test]
fn a_bare_list_of_urls_is_an_m3u() {
    let entries = parse_playlist("\u{feff}http://a.example/stream\n").unwrap();
    assert_eq!(urls(&entries), ["http://a.example/stream"]);
}

#[test]
fn a_pls_pairs_titles_with_files_by_number() {
    let text = "[Playlist]\nNumberOfEntries=2\nFile2=http://mirror.example/live\nTitle1=Jazz FM\nfile1=http://main.example/live\nLength1=-1\nVersion=2\n";
    let entries = parse_playlist(text).unwrap();
    assert_eq!(
        urls(&entries),
        ["http://main.example/live", "http://mirror.example/live"]
    );
    assert_eq!(entries[0].title.as_deref(), Some("Jazz FM"));
    assert_eq!(entries[1].title, None);
}

#[test]
fn hls_is_recognised_and_refused() {
    let text = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg1.ts\n";
    assert_eq!(parse_playlist(text), Err(PlaylistError::Hls));
}

#[test]
fn a_playlist_naming_nothing_is_empty() {
    assert_eq!(
        parse_playlist("#EXTM3U\n# nothing\n"),
        Err(PlaylistError::Empty)
    );
    assert_eq!(
        parse_playlist("[playlist]\nNumberOfEntries=0\n"),
        Err(PlaylistError::Empty)
    );
    assert_eq!(parse_playlist(""), Err(PlaylistError::Empty));
}

#[test]
fn relative_entries_are_kept_for_the_caller_to_resolve() {
    let entries = parse_playlist("#EXTM3U\nlive.mp3\n").unwrap();
    assert_eq!(urls(&entries), ["live.mp3"]);
    assert!(!is_http_url(&entries[0].url));
}

#[test]
fn only_http_and_https_are_stations() {
    assert!(is_http_url("HTTP://a.example/x"));
    assert!(is_http_url(" https://a.example/x"));
    assert!(!is_http_url("file:///etc/passwd"));
    assert!(!is_http_url("rtsp://a.example/x"));
    assert!(!is_http_url("C:\\music\\a.mp3"));
}

#[test]
fn playlist_extensions_are_matched_without_case() {
    assert!(has_playlist_extension("/listen.PLS"));
    assert!(has_playlist_extension("live.m3u8"));
    assert!(!has_playlist_extension("/stream.mp3"));
}
