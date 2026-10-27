use super::*;

#[test]
fn hls_is_recognised_by_its_url_or_its_type() {
    assert!(names_hls("https://a/live/index.m3u8"));
    assert!(names_hls("https://a/live/INDEX.M3U8?token=x"));
    assert!(!names_hls("https://a/stream.mp3"));
    assert!(!names_hls("https://a/m3u8/stream"));
    assert!(is_mpegurl(Some(
        "application/vnd.apple.mpegurl; charset=utf-8"
    )));
    assert!(is_mpegurl(Some("audio/x-mpegurl")));
    assert!(!is_mpegurl(Some("audio/mpeg")));
    assert!(!is_mpegurl(None));
}

fn segments(uris: &[&str], first: u64) -> Vec<hls::Segment> {
    uris.iter()
        .enumerate()
        .map(|(i, uri)| hls::Segment {
            uri: uri.to_string(),
            duration_s: 6.0,
            sequence: first + i as u64,
        })
        .collect()
}

#[test]
fn a_numbered_window_is_followed_and_a_stall_skips_ahead() {
    let window = segments(&["a", "b", "c", "d"], 100);
    assert_eq!(resync(102, Some("b"), &window), 102);
    // Nothing new: wait.
    assert_eq!(resync(104, Some("d"), &window), 104);
    // The window moved past what was expected.
    assert_eq!(resync(90, Some("x"), &window), 100);
}

#[test]
fn an_unnumbered_or_restarted_playlist_is_picked_up_again() {
    // Numbers nothing: every reload starts at 0, the last taken is "c".
    let reload = segments(&["b", "c", "d", "e"], 0);
    assert_eq!(resync(3, Some("c"), &reload), 2);
    // Nothing new after "e": wait.
    assert_eq!(resync(4, Some("e"), &reload), 4);
    // An encoder that restarted at 0 with new names: near the edge.
    let restarted = segments(&["n0", "n1", "n2", "n3", "n4"], 0);
    assert_eq!(resync(500, Some("old"), &restarted), 2);
}

#[test]
fn playlist_entries_resolve_against_the_playlist() {
    assert_eq!(
        resolve(true, "https://cdn.example/live/hi/index.m3u8", "seg-1.ts").unwrap(),
        "https://cdn.example/live/hi/seg-1.ts"
    );
    assert_eq!(
        resolve(true, "https://cdn.example/live/master.m3u8", "/abs/x.m3u8").unwrap(),
        "https://cdn.example/abs/x.m3u8"
    );
    assert!(resolve(true, "https://cdn.example/a.m3u8", "file:///etc/passwd").is_err());
    // A public playlist cannot send the player into the local network;
    // a station on it can name its own segments.
    assert!(resolve(
        true,
        "https://cdn.example/a.m3u8",
        "http://192.168.1.1/x.ts"
    )
    .is_err());
    assert!(resolve(true, "https://cdn.example/a.m3u8", "http://router.lan/x.ts").is_err());
    assert!(resolve(false, "http://192.168.1.20/a.m3u8", "x.ts").is_ok());
}

#[test]
fn split_artist_title_splits_on_separator() {
    assert_eq!(
        split_artist_title("Daft Punk - Get Lucky"),
        ("Get Lucky".to_string(), Some("Daft Punk".to_string()))
    );
    // No separator → whole string is the title, no artist.
    assert_eq!(
        split_artist_title("Morning Show"),
        ("Morning Show".to_string(), None)
    );
    // A dangling separator half keeps the raw string as the title.
    assert_eq!(
        split_artist_title("Artist - "),
        ("Artist - ".to_string(), None)
    );
}
