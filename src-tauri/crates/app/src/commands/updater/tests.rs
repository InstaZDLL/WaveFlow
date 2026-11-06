use super::{
    install_order, is_newer, UpdateChannel, BETA_ENDPOINT, BETA_MIRROR_ENDPOINT, STABLE_ENDPOINT,
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

/// GitHub first, its mirror second: when both announce the same release,
/// both are kept, in the order they were read, so a payload GitHub cannot
/// deliver is fetched from R2 instead.
#[test]
fn the_same_version_from_both_hosts_is_kept_in_reading_order() {
    assert_eq!(install_order(&["2.0.1", "2.0.1"]), [0, 1]);
}

/// A host behind is no fallback: installing its older release after the
/// newer one failed would offer an update the user did not choose.
#[test]
fn an_older_version_is_never_a_fallback() {
    assert_eq!(install_order(&["2.0.0", "2.0.1"]), [1]);
    assert_eq!(install_order(&["2.0.1", "2.0.0"]), [0]);
    // The beta channel's four manifests: a newer stable outranks the
    // beta on both hosts, and both copies of it are kept.
    assert_eq!(
        install_order(&["2.1.0-beta.2", "2.1.0-beta.2", "2.1.0", "2.1.0"]),
        [2, 3]
    );
}

#[test]
fn build_metadata_does_not_split_the_candidates() {
    assert_eq!(install_order(&["2.0.1+gh", "2.0.1+r2"]), [0, 1]);
}

#[test]
fn nothing_found_is_nothing_to_install() {
    assert!(install_order(&[]).is_empty());
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
