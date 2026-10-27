use super::*;

/// The panel names the genre the way the track carries it, not the
/// pattern it matched: "Lo-Fi Hip Hop", not "lo-fi".
#[test]
fn the_excluded_genre_is_named_as_tagged() {
    let defaults: Vec<String> = excluded_from(None);
    assert_eq!(
        first_excluded_genre(["Jazz", "Lo-Fi Hip Hop"], &defaults),
        Some("Lo-Fi Hip Hop")
    );
    assert_eq!(first_excluded_genre(["Jazz", "Trap"], &defaults), None);
}

#[test]
fn the_default_genres_catch_their_variants() {
    let defaults: Vec<String> = excluded_from(None);
    for genre in [
        "Instrumental",
        "instrumentals",
        "Instrumental Hip-Hop",
        "Lo-Fi",
        "Lofi",
        "Lo Fi",
        "lo-fi hip hop",
        "LOFI BEATS",
        "Chillhop / Lo-fi",
    ] {
        assert!(
            any_genre_excluded([genre], &defaults),
            "{genre} should be excluded by default"
        );
    }
}

#[test]
fn a_pattern_matches_whole_words_only() {
    // A substring test would exclude every trap track for "rap".
    assert!(!genre_matches("Trap", "rap"));
    assert!(!genre_matches("Folk", "lofi"));
    assert!(!genre_matches("Instrumentalism", "instrumental"));
    assert!(genre_matches("French Rap", "rap"));
    assert!(!any_genre_excluded(
        ["Pop", "Hip-Hop"],
        &excluded_from(None)
    ));
}

#[test]
fn a_blank_pattern_excludes_nothing() {
    assert!(!genre_matches("Pop", ""));
    assert!(!genre_matches("Pop", " - "));
}

#[test]
fn an_empty_list_is_a_choice_but_junk_is_not() {
    assert!(excluded_from(Some("[]")).is_empty());
    assert_eq!(excluded_from(Some("not json")), excluded_from(None));
    assert!(disabled_from(Some("[]")).is_empty());
    assert_eq!(disabled_from(Some("{")), DEFAULT_DISABLED.to_vec());
}

#[test]
fn genius_is_off_until_turned_on() {
    assert_eq!(disabled_from(None), vec![Provider::Genius]);
}

#[test]
fn lrclib_cannot_be_switched_off() {
    let disabled = disabled_from(Some(r#"["lrclib","megalobiz","nope"]"#));
    assert_eq!(disabled, vec![Provider::Megalobiz]);
}
