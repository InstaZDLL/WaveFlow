use super::*;

#[test]
fn https_upgrades_to_wss_and_http_to_ws() {
    assert_eq!(
        ws_url("https://music.example", 12).unwrap(),
        "wss://music.example/api/v2/sync/socket?after=12"
    );
    assert_eq!(
        ws_url("http://localhost:4533", 0).unwrap(),
        "ws://localhost:4533/api/v2/sync/socket?after=0"
    );
}

#[test]
fn a_trailing_slash_does_not_double_up_the_path() {
    assert_eq!(
        ws_url("https://music.example/", 1).unwrap(),
        "wss://music.example/api/v2/sync/socket?after=1"
    );
}

#[test]
fn the_hosts_case_survives_for_a_case_sensitive_proxy() {
    assert!(ws_url("https://Music.Example/Path", 1)
        .unwrap()
        .starts_with("wss://Music.Example/Path"));
}

#[test]
fn a_scheme_we_cannot_upgrade_is_an_error() {
    assert!(ws_url("ftp://music.example", 0).is_err());
    assert!(ws_url("music.example", 0).is_err());
}

#[test]
fn cleartext_is_only_flagged_when_it_leaves_the_machine() {
    assert!(!is_cleartext_to_a_remote_host(
        "ws://localhost:4533/api/v2/sync/socket?after=0"
    ));
    assert!(!is_cleartext_to_a_remote_host(
        "ws://127.0.0.1:4533/api/v2/sync/socket?after=0"
    ));
    assert!(!is_cleartext_to_a_remote_host("ws://[::1]:4533/x"));
    assert!(is_cleartext_to_a_remote_host("ws://nas.lan/x"));
    assert!(is_cleartext_to_a_remote_host("ws://198.51.100.7/x"));
    // Encrypted is never flagged, wherever it points.
    assert!(!is_cleartext_to_a_remote_host("wss://music.example/x"));
}

#[test]
fn an_unparseable_url_is_treated_as_remote() {
    assert!(is_cleartext_to_a_remote_host("ws://"));
}

#[test]
fn a_notice_frame_yields_its_cursor() {
    assert_eq!(notice_cursor(r#"{"cursor": 43}"#), Some(43));
}

#[test]
fn a_frame_we_cannot_read_is_not_fatal() {
    // The caller fetches regardless, so an unreadable frame costs
    // nothing but the parse attempt.
    assert_eq!(notice_cursor("not json"), None);
    assert_eq!(notice_cursor(r#"{"other": 1}"#), None);
    assert_eq!(notice_cursor(r#"{"cursor": "43"}"#), None);
}
