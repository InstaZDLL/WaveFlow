use super::{is_newer, UpdateChannel, BETA_ENDPOINT, STABLE_ENDPOINT};

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
    assert_eq!(UpdateChannel::Stable.endpoints(), [STABLE_ENDPOINT]);
    assert_eq!(
        UpdateChannel::Beta.endpoints(),
        [BETA_ENDPOINT, STABLE_ENDPOINT]
    );
}
