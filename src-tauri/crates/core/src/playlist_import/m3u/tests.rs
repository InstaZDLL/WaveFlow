use super::*;

#[test]
fn extinf_lines_name_the_entry_that_follows() {
    let text = "\u{feff}#EXTM3U\n#PLAYLIST:Evening\n#EXTINF:225,Daft Punk - One More Time\n\
                C:\\Music\\one.flac\nrelative/two.mp3\n#EXTINF:-1,Just a title\nthree.ogg\n";
    let m3u = parse(text);
    assert_eq!(m3u.name.as_deref(), Some("Evening"));
    assert_eq!(m3u.entries.len(), 3);
    assert_eq!(
        m3u.entries[0],
        M3uEntry {
            path: "C:\\Music\\one.flac".into(),
            title: Some("One More Time".into()),
            artist: Some("Daft Punk".into()),
            duration_ms: Some(225_000),
        }
    );
    // No #EXTINF: the line before it belonged to the previous entry.
    assert_eq!(m3u.entries[1].title, None);
    assert_eq!(m3u.entries[2].title.as_deref(), Some("Just a title"));
    assert_eq!(m3u.entries[2].artist, None);
    assert_eq!(m3u.entries[2].duration_ms, None);
}

#[test]
fn extinf_attributes_are_skipped() {
    let (duration, display) = parse_extinf("123 tvg-id=\"x\",A - B");
    assert_eq!(duration, Some(123_000));
    assert_eq!(display.as_deref(), Some("A - B"));
}
