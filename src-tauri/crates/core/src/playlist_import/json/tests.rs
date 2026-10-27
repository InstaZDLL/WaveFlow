use super::*;

#[test]
fn an_object_with_a_name_and_rich_tracks() {
    let text = r#"{
            "name": "Road trip",
            "tracks": [
                {"title": "One", "artists": [{"name": "A"}, {"name": "B"}],
                 "album": {"title": "First"}, "duration_ms": 200000, "isrc": "us-rc1-76-07839"},
                {"name": "Two", "artist": "C", "album": "Second", "duration": "3:20"},
                "not a track",
                {"artist": "no title"}
            ]
        }"#;
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.name.as_deref(), Some("Road trip"));
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(parsed.entries[0].artist.as_deref(), Some("A; B"));
    assert_eq!(parsed.entries[0].album.as_deref(), Some("First"));
    assert_eq!(parsed.entries[0].duration_ms, Some(200_000));
    assert_eq!(parsed.entries[0].isrc.as_deref(), Some("USRC17607839"));
    assert_eq!(parsed.entries[1].title, "Two");
    assert_eq!(parsed.entries[1].duration_ms, Some(200_000));
}

#[test]
fn a_bare_array_with_durations_in_seconds() {
    let parsed = parse(r#"[{"title": "X", "artist": ["Y"], "duration": 61.5}]"#).unwrap();
    assert_eq!(parsed.name, None);
    assert_eq!(parsed.entries[0].artist.as_deref(), Some("Y"));
    assert_eq!(parsed.entries[0].duration_ms, Some(61_500));
}

#[test]
fn what_is_not_a_playlist_is_refused() {
    assert!(matches!(parse("{"), Err(ParseError::Json(_))));
    assert!(matches!(parse(r#"{"x": 1}"#), Err(ParseError::Json(_))));
    assert!(matches!(parse("42"), Err(ParseError::Json(_))));
    assert_eq!(parse("[]"), Err(ParseError::Empty));
}
