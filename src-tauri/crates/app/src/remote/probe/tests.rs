use super::*;

/// Verbatim body from a live server, missing-parameter branch.
const WAVEFLOW_400: &str = r#"{"subsonic-response":{"error":{"code":10,"message":"Required parameter is missing"},"openSubsonic":true,"serverVersion":"2.0.0-beta.0","status":"failed","type":"waveflow","version":"1.16.1"}}"#;

/// Verbatim body from a live server, bad-credentials branch.
const WAVEFLOW_401: &str = r#"{"subsonic-response":{"error":{"code":40,"message":"Wrong username or password"},"openSubsonic":true,"serverVersion":"2.0.0-beta.0","status":"failed","type":"waveflow","version":"1.16.1"}}"#;

#[test]
fn a_failed_ping_still_identifies_a_waveflow_server() {
    // This is the whole point of the probe: both of these arrive
    // with a non-2xx status, and both are enough to decide.
    for body in [WAVEFLOW_400, WAVEFLOW_401] {
        let flavour = classify_ping_body(body).unwrap();
        assert_eq!(
            flavour,
            ServerFlavour::Waveflow {
                server_version: Some("2.0.0-beta.0".into())
            }
        );
        assert!(flavour.supports_sync());
    }
}

#[test]
fn a_third_party_server_is_accepted_without_the_sync_capability() {
    let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"someserver","serverVersion":"1.2.3"}}"#;
    let flavour = classify_ping_body(body).unwrap();
    assert_eq!(
        flavour,
        ServerFlavour::Subsonic {
            server_type: Some("someserver".into()),
            server_version: Some("1.2.3".into()),
        }
    );
    assert!(!flavour.supports_sync());
}

#[test]
fn a_server_that_omits_its_type_is_treated_as_third_party() {
    // Never assume a capability we have not observed: no `type`
    // means no evidence of the journal.
    let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
    let flavour = classify_ping_body(body).unwrap();
    assert!(!flavour.supports_sync());
}

#[test]
fn the_type_comparison_ignores_case() {
    let body = r#"{"subsonic-response":{"status":"ok","type":"WaveFlow"}}"#;
    assert!(classify_ping_body(body).unwrap().supports_sync());
}

#[test]
fn something_that_is_not_a_music_server_is_an_error() {
    assert!(classify_ping_body("<html>404</html>").is_err());
    assert!(classify_ping_body("{}").is_err());
    assert!(classify_ping_body(r#"{"hello":"world"}"#).is_err());
}
