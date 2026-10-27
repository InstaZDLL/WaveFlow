use super::*;

#[test]
fn parses_and_respells_canonically() {
    let combo = Combo::parse("Shift+Ctrl+p").unwrap();
    assert_eq!(combo.canonical(), "Ctrl+Shift+P");
    assert_eq!(
        Combo::parse("Super+Alt+ArrowUp").unwrap().canonical(),
        "Alt+Super+ArrowUp"
    );
}

#[test]
fn refuses_what_would_steal_a_key_from_every_application() {
    assert_eq!(Combo::parse("Space"), Err(ComboError::NeedsModifier));
    assert_eq!(Combo::parse("Shift+S"), Err(ComboError::NeedsModifier));
    assert_eq!(Combo::parse("F5"), Err(ComboError::NeedsModifier));
    assert!(Combo::parse("F13").is_ok());
    assert!(Combo::parse("Shift+F24").is_ok());
    assert!(Combo::parse("Super+M").is_ok());
}

#[test]
fn refuses_unknown_keys_and_empty_combinations() {
    assert_eq!(Combo::parse(""), Err(ComboError::Empty));
    assert_eq!(Combo::parse("Ctrl+"), Err(ComboError::Empty));
    assert_eq!(Combo::parse("Ctrl+Alt"), Err(ComboError::Empty));
    assert_eq!(Combo::parse("Ctrl+;"), Err(ComboError::UnsupportedKey));
    assert_eq!(Combo::parse("Hyper+P"), Err(ComboError::UnsupportedKey));
    assert_eq!(
        Combo::parse("Ctrl+MediaPlayPause"),
        Err(ComboError::UnsupportedKey)
    );
}

#[test]
fn native_shortcut_carries_the_modifiers_and_the_code() {
    let shortcut = Combo::parse("Ctrl+Alt+P").unwrap().shortcut();
    assert_eq!(shortcut.mods, Modifiers::CONTROL | Modifiers::ALT);
    assert_eq!(shortcut.key, Code::KeyP);
    let bare = Combo::parse("F13").unwrap().shortcut();
    assert!(bare.mods.is_empty());
    assert_eq!(bare.key, Code::F13);
}

#[test]
fn portal_trigger_follows_the_shortcuts_specification() {
    assert_eq!(
        Combo::parse("Ctrl+Shift+Super+P").unwrap().portal_trigger(),
        "CTRL+SHIFT+LOGO+p"
    );
    assert_eq!(
        Combo::parse("Alt+PageDown").unwrap().portal_trigger(),
        "ALT+Page_Down"
    );
    assert_eq!(
        Combo::parse("Ctrl+NumpadAdd").unwrap().portal_trigger(),
        "CTRL+KP_Add"
    );
}

#[test]
fn every_token_is_unique() {
    let mut tokens: Vec<_> = KEYS.iter().map(|k| k.token.to_ascii_lowercase()).collect();
    tokens.sort();
    let len = tokens.len();
    tokens.dedup();
    assert_eq!(tokens.len(), len);
}
