use super::{
    is_newer, UpdateChannel, BETA_ENDPOINT, BETA_MIRROR_ENDPOINT, STABLE_ENDPOINT,
    STABLE_MIRROR_ENDPOINT,
};

#[test]
fn a_release_outranks_its_own_betas() {
    assert!(is_newer("1.8.0", "1.8.0-beta.3"));
    assert!(!is_newer("1.8.0-beta.3", "1.8.0"));
    assert!(is_newer("1.8.0-beta.10", "1.8.0-beta.3"));
}

#[test]
fn an_older_release_does_not_displace_a_newer_beta() {
    assert!(!is_newer("1.8.1", "1.9.0-beta.1"));
    assert!(is_newer("1.9.0-beta.1", "1.8.1"));
}

#[test]
fn build_metadata_does_not_make_a_version_newer() {
    assert!(!is_newer("1.8.0+z", "1.8.0+a"));
    assert!(!is_newer("1.8.0+a", "1.8.0+z"));
}

#[test]
fn a_version_that_does_not_parse_never_wins() {
    assert!(!is_newer("not-a-version", "1.8.0"));
    assert!(is_newer("1.8.0", "not-a-version"));
}

#[test]
fn the_beta_channel_reads_the_stable_manifest_too() {
    assert_eq!(
        UpdateChannel::Stable.endpoints(),
        [STABLE_ENDPOINT, STABLE_MIRROR_ENDPOINT]
    );
    assert_eq!(
        UpdateChannel::Beta.endpoints(),
        [
            BETA_ENDPOINT,
            BETA_MIRROR_ENDPOINT,
            STABLE_ENDPOINT,
            STABLE_MIRROR_ENDPOINT
        ]
    );
}

/// A build that lost its mirror would stop updating the day GitHub
/// stops answering, and nothing would say so before that day.
#[test]
fn every_channel_reads_github_and_its_mirror() {
    for channel in [UpdateChannel::Stable, UpdateChannel::Beta] {
        let endpoints = channel.endpoints();
        assert!(endpoints
            .iter()
            .any(|e| e.starts_with("https://github.com/")));
        assert!(endpoints
            .iter()
            .any(|e| e.starts_with("https://download.waveflow.app/")));
    }
}

/// `tauri.conf.json` names the stable pair too. Nothing reads it while
/// every check goes through `UpdateChannel::endpoints`, so only this
/// test keeps the two lists from drifting apart.
#[test]
fn the_static_config_names_the_stable_pair() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../../../tauri.conf.json")).unwrap();
    let endpoints: Vec<&str> = config["plugins"]["updater"]["endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(endpoints, UpdateChannel::Stable.endpoints());
}

/// A manifest is not signed, only the payload is. Without the version
/// bound into the payload's signature, either host could announce a high
/// version over an older, genuinely signed build and walk every
/// installation back to it — and the newest answer is the one kept.
#[test]
fn the_payload_signature_must_name_the_announced_version() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../../../tauri.conf.json")).unwrap();
    assert_eq!(
        config["plugins"]["updater"]["requireSignedVersion"],
        serde_json::Value::Bool(true)
    );
}
