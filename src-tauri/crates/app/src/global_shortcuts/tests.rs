use super::*;

#[test]
fn stored_row_round_trips_and_keeps_what_no_longer_parses() {
    let parsed = parse_stored(
        r#"{"togglePlayback":"shift+ctrl+p","next":"Ctrl+;","fromTheFuture":"Ctrl+X","volumeUp":""}"#,
    );
    assert_eq!(
        parsed
            .get(&GlobalAction::TogglePlayback)
            .map(String::as_str),
        Some("Ctrl+Shift+P")
    );
    // Kept as typed, to be shown as invalid.
    assert_eq!(
        parsed.get(&GlobalAction::Next).map(String::as_str),
        Some("Ctrl+;")
    );
    assert!(!parsed.contains_key(&GlobalAction::VolumeUp));
    assert_eq!(parsed.len(), 2);
    assert!(parse_stored("not json").is_empty());
}

#[test]
fn action_ids_match_their_serialized_form() {
    for action in GlobalAction::ALL {
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, format!("\"{}\"", action.id()));
        assert_eq!(GlobalAction::from_id(action.id()), Some(action));
    }
}

#[test]
fn a_combination_held_elsewhere_reads_as_taken() {
    let hotkey = Combo::parse("Ctrl+Alt+P").unwrap().shortcut();
    let err =
        tauri_plugin_global_shortcut::Error::from(global_hotkey::Error::AlreadyRegistered(hotkey));
    assert_eq!(classify(&err), BindingState::Taken);
    let other = tauri_plugin_global_shortcut::Error::from(global_hotkey::Error::FailedToRegister(
        "RegisterEventHotKey failed for P".into(),
    ));
    assert!(matches!(classify(&other), BindingState::Refused { .. }));
}
