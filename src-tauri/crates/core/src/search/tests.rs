use super::*;

fn indexed(q: &str) -> Vec<String> {
    match plan_search(q) {
        Some(SearchPlan::Indexed(t)) => t,
        other => panic!("expected an indexed plan for {q:?}, got {other:?}"),
    }
}

fn scanned(q: &str) -> Vec<String> {
    match plan_search(q) {
        Some(SearchPlan::Scanned(t)) => t,
        other => panic!("expected a scanned plan for {q:?}, got {other:?}"),
    }
}

#[test]
fn nothing_to_search_is_not_a_plan() {
    assert_eq!(plan_search(""), None);
    assert_eq!(plan_search("   "), None);
    // Quotes are dropped before the split, so a query of nothing but
    // quotes has no terms left.
    assert_eq!(plan_search("\"\""), None);
}

#[test]
fn ordinary_words_go_to_the_index() {
    assert_eq!(indexed("moon"), ["moon"]);
    assert_eq!(indexed("dark side"), ["dark", "side"]);
}

/// The defect this exists for: three Han characters are three
/// characters, not nine bytes, and they belong on the fast route.
#[test]
fn han_characters_are_counted_as_characters() {
    assert_eq!(indexed("人民音"), ["人民音"]);
    // Two of them cannot be a trigram, so they take the scan — which
    // is the common shape of a Chinese word, not an edge case.
    assert_eq!(scanned("人民"), ["人民"]);
}

/// A short term anywhere sends the whole query down the scan,
/// including the terms that would have been fine on their own.
#[test]
fn one_short_term_decides_for_the_whole_query() {
    assert_eq!(scanned("u2 sunday"), ["u2", "sunday"]);
}

/// `U2` and `M83` matched before trigram (a prefix on a whole token)
/// and must keep matching after it, or this would be a regression
/// for short Latin names.
#[test]
fn short_latin_names_still_reach_a_route() {
    assert_eq!(scanned("u2"), ["u2"]);
    assert_eq!(scanned("ok"), ["ok"]);
}

/// An embedded quote would close the phrase the SQLite layer builds
/// and leave the rest of the query to be read as FTS5 syntax.
#[test]
fn embedded_quotes_are_dropped() {
    assert_eq!(indexed("foo\"bar"), ["foobar"]);
}
