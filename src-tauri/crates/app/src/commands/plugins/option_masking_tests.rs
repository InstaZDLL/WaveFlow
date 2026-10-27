use super::*;
use std::collections::HashMap;
use waveflow_core::plugin::manifest::{LocalizedString, OptionDecl};

fn decl(key: &str, option_type: &str, sensitive: bool) -> OptionDecl {
    OptionDecl {
        key: key.into(),
        option_type: option_type.into(),
        label: LocalizedString::Plain(key.into()),
        label_i18n: None,
        default: None,
        choices: vec!["a".into(), "b".into()],
        description: None,
        description_i18n: None,
        sensitive,
    }
}

fn stored(key: &str, value: &str) -> HashMap<String, String> {
    HashMap::from([(key.to_string(), value.to_string())])
}

/// The property the flag exists for: a stored credential does not
/// travel to the webview, only the fact that there is one.
#[test]
fn a_stored_credential_never_leaves_the_backend() {
    let opt = to_plugin_option(decl("sp_dc", "text", true), &stored("sp_dc", "secret"));
    assert_eq!(opt.value, None);
    assert!(opt.is_set);
    assert!(opt.sensitive);
}

#[test]
fn an_empty_credential_is_not_set() {
    let opt = to_plugin_option(decl("sp_dc", "text", true), &stored("sp_dc", ""));
    assert!(!opt.is_set);
}

#[test]
fn a_plain_text_option_still_shows_its_value() {
    let opt = to_plugin_option(decl("lang", "text", false), &stored("lang", "fr"));
    assert_eq!(opt.value.as_deref(), Some("fr"));
}

#[test]
fn only_a_text_option_can_be_masked() {
    let opt = to_plugin_option(decl("mode", "enum", true), &stored("mode", "b"));
    assert_eq!(opt.value.as_deref(), Some("b"));
    assert!(!opt.sensitive);
}
