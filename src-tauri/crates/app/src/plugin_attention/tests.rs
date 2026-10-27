use super::*;

#[test]
fn only_the_prefixed_plugin_error_asks_for_a_credential() {
    assert!(is_auth_required(&SourceError::Plugin(
        "auth-required: sp_dc refused".into()
    )));
    assert!(!is_auth_required(&SourceError::Plugin(
        "network timeout".into()
    )));
    assert!(!is_auth_required(&SourceError::Trap(
        "auth-required: not from a trap".into()
    )));
}
