use super::*;

#[test]
fn redact_strips_userinfo_and_query() {
    assert_eq!(
        redact_url("https://user:secret@host.example/path?token=xyz"),
        "https://host.example/path"
    );
    assert_eq!(
        redact_url("https://lrclib.net/api/get?artist_name=a&track_name=b"),
        "https://lrclib.net/api/get"
    );
    assert_eq!(redact_url("not a url"), "<unparseable url>");
}

#[test]
fn host_allowlist_round_trips() {
    assert!(host_is_allowed("apic-desktop.musixmatch.com"));
    assert!(host_is_allowed("lrclib.net"));
    // Suffix match for CDNs.
    assert!(host_is_allowed("static.musixmatch.com"));
    assert!(host_is_allowed("img.genius.com"));
    // Adjacent but distinct hosts must not slip through.
    assert!(!host_is_allowed("musixmatch.com.attacker.com"));
    assert!(!host_is_allowed("evil.com"));
}

#[test]
fn validate_outbound_url_rejects_off_host() {
    let allowed = &["genius.com"];
    assert!(validate_outbound_url("https://genius.com/song", allowed).is_ok());
    assert!(validate_outbound_url("https://evil.com/song", allowed).is_err());
    assert!(validate_outbound_url("ftp://genius.com/song", allowed).is_err());
    assert!(validate_outbound_url("file:///etc/passwd", allowed).is_err());
    assert!(validate_outbound_url("garbage", allowed).is_err());
}
