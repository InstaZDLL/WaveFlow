use super::*;

#[test]
fn rules_json_carries_on_repeat_kind() {
    let json = SmartPlaylistRules::OnRepeat.to_json().expect("serialize");
    assert!(
        json.contains("\"kind\":\"on_repeat\""),
        "OnRepeat serialized incorrectly: {json}"
    );
}
