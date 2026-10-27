use super::*;

#[test]
fn defaults_are_off_and_on_the_standard_port() {
    let cfg = MpdConfig::default();
    assert!(!cfg.enabled, "the server must stay off until opted into");
    assert_eq!(cfg.port, 6600);
    assert!(!cfg.requires_auth());
}

#[test]
fn a_non_empty_password_turns_auth_on() {
    let cfg = MpdConfig {
        password: "hunter2".into(),
        ..Default::default()
    };
    assert!(cfg.requires_auth());
}
