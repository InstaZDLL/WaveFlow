use super::*;

#[test]
fn full_links_and_bare_ids_give_the_playlist_id() {
    for input in [
        "https://www.deezer.com/fr/playlist/1109890291",
        "https://www.deezer.com/playlist/1109890291?utm_source=x",
        "deezer.com/en/playlist/1109890291/",
        "  1109890291 ",
        "http://WWW.DEEZER.COM/playlist/1109890291#top",
    ] {
        assert_eq!(
            parse(input),
            Some(DeezerLink::Playlist(1_109_890_291)),
            "{input}"
        );
    }
}

#[test]
fn share_links_are_followed_not_read() {
    assert_eq!(
        parse("https://link.deezer.com/s/30ABCdef"),
        Some(DeezerLink::Share(
            "https://link.deezer.com/s/30ABCdef".into()
        ))
    );
    assert_eq!(parse("https://link.deezer.com/"), None);
}

#[test]
fn anything_else_is_refused() {
    for input in [
        "",
        "https://www.deezer.com/fr/album/302127",
        "https://evil.example/playlist/1",
        "https://www.deezer.com.evil.example/playlist/1",
        "https://user@www.deezer.com/playlist/1",
        "https://www.deezer.com:8443/playlist/1",
        "https://www.deezer.com/playlist/abc",
        "https://www.deezer.com/playlist/0",
    ] {
        assert_eq!(parse(input), None, "{input}");
    }
    assert!(is_deezer_host("https://www.deezer.com/fr/playlist/1"));
    assert!(!is_deezer_host("https://deezer.com.evil.example/"));
}
