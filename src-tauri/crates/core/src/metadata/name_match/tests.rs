use super::*;

fn pick<'a>(names: &[&'a str], searched: &str) -> Option<&'a str> {
    select_by_name(names.iter().copied(), &normalize_name(searched), |n| {
        Some(*n)
    })
}

#[test]
fn normalize_folds_punctuation_case_and_diacritics() {
    assert_eq!(normalize_name("Boney M."), "boney m");
    assert_eq!(normalize_name("Boney M"), "boney m");
    assert_eq!(
        normalize_name("Bob Marley & The Wailers"),
        "bob marley and the wailers"
    );
    assert_eq!(normalize_name("Beyoncé"), "beyonce");
    assert_eq!(normalize_name("Guns N' Roses"), "guns n roses");
    // Non-Latin script survives (minus punctuation).
    assert_eq!(normalize_name("宇多田ヒカル"), "宇多田ヒカル");
}

#[test]
fn normalize_is_stable_across_nfc_and_nfd_forms() {
    // "Björk": precomposed ö (NFC) vs o + combining diaeresis (NFD).
    let bjork_nfc = "Bj\u{00F6}rk";
    let bjork_nfd = "Bjo\u{0308}rk";
    assert_eq!(normalize_name(bjork_nfc), "bjork");
    assert_eq!(normalize_name(bjork_nfd), "bjork");
    assert_eq!(normalize_name(bjork_nfc), normalize_name(bjork_nfd));

    // "Beyoncé": precomposed é (NFC) vs e + combining acute (NFD).
    let beyonce_nfc = "Beyonc\u{00E9}";
    let beyonce_nfd = "Beyonce\u{0301}";
    assert_eq!(normalize_name(beyonce_nfd), "beyonce");
    assert_eq!(normalize_name(beyonce_nfc), normalize_name(beyonce_nfd));
}

#[test]
fn exact_match_wins_regardless_of_position() {
    // The relevant entry sits after a homonym; exact match must still win.
    assert_eq!(
        pick(&["Nirvana (60s band)", "Nirvana"], "Nirvana"),
        Some("Nirvana")
    );
}

#[test]
fn accentless_library_name_matches_accented_entry() {
    // TheAudioDB / Deezer search is accent-insensitive, so "Celine
    // Dion" (no accent) still returns "Céline Dion"; the client match
    // must survive the accent difference — jo-el414's report on #342.
    assert_eq!(pick(&["Céline Dion"], "Celine Dion"), Some("Céline Dion"));
}

#[test]
fn punctuation_only_difference_matches() {
    assert_eq!(pick(&["Boney M"], "Boney M."), Some("Boney M"));
}

#[test]
fn superset_name_matches_via_connector() {
    assert_eq!(
        pick(&["Bob Marley & The Wailers"], "Bob Marley"),
        Some("Bob Marley & The Wailers")
    );
}

#[test]
fn superset_search_matches_subset_candidate() {
    // Reverse of `superset_name_matches_via_connector`: the *search*
    // is the long canonical name and the catalogue only carries the
    // short one. Exercises the (short, long) swap in
    // `names_prefix_compatible` from the other side.
    assert_eq!(
        pick(&["Bob Marley"], "Bob Marley & The Wailers"),
        Some("Bob Marley")
    );
}

#[test]
fn prefers_exact_over_prefix() {
    assert_eq!(
        pick(&["Bob Marley & The Wailers", "Bob Marley"], "Bob Marley"),
        Some("Bob Marley")
    );
}

#[test]
fn rejects_unrelated_homonym() {
    assert_eq!(pick(&["Bob Dylan"], "Bob Marley"), None);
    assert!(!names_prefix_compatible("bob marley", "bob dylan"));
}

#[test]
fn rejects_parenthetical_homonym_when_alone() {
    // "Nirvana (60s band)" alone must NOT satisfy a "Nirvana" search —
    // the " 60s band" suffix is a disambiguator, not a connector.
    assert_eq!(pick(&["Nirvana (60s band)"], "Nirvana"), None);
    assert!(!names_prefix_compatible(
        "nirvana",
        &normalize_name("Nirvana (60s band)")
    ));
    // But a real connector suffix still matches.
    assert!(names_prefix_compatible(
        "nirvana",
        "nirvana and the deep sea"
    ));
}

#[test]
fn empty_search_is_none() {
    assert_eq!(pick(&["Anything"], ""), None);
}

#[test]
fn names_match_is_the_yes_no_form_of_the_selector() {
    assert!(names_match("Céline Dion", "Celine Dion"));
    assert!(names_match("Bob Marley", "Bob Marley & The Wailers"));
    assert!(names_match("Bob Marley & The Wailers", "Bob Marley"));
    assert!(!names_match("Nirvana", "Nirvana (60s band)"));
    assert!(!names_match("", ""));
    assert!(!names_match("Air", "Airbourne"));
}
