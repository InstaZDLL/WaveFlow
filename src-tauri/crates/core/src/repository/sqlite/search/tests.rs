use super::*;
use crate::search::plan_search;

/// The clause and its bind count must agree — a mismatch shifts
/// every following positional parameter without any error.
#[test]
fn the_like_clause_has_one_placeholder_per_declared_bind() {
    assert_eq!(LIKE_TERM_CLAUSE.matches('?').count(), LIKE_TERM_BINDS);
    // And the escape character it names is the one `like_pattern`
    // escapes with.
    assert!(LIKE_TERM_CLAUSE.contains(&format!("ESCAPE '{LIKE_ESCAPE}'")));
}

#[test]
fn terms_become_quoted_phrases_joined_by_and() {
    let plan = plan_search("dark side").unwrap();
    assert_eq!(fts5_expression(plan.terms()), "\"dark\" \"side\"");
}

/// FTS5 reads bare `*`, `^`, `:`, `AND`, `NOT` as syntax. Quoting
/// every term keeps a query of `AND` a search for the word.
#[test]
fn fts_operators_are_neutralised_by_quoting() {
    assert_eq!(fts5_expression(&["AND".to_owned()]), "\"AND\"");
    assert_eq!(fts5_expression(&["foo*".to_owned()]), "\"foo*\"");
}

/// A `%` that reached LIKE unescaped would match the entire library.
#[test]
fn like_wildcards_in_user_input_are_escaped() {
    assert_eq!(like_pattern("100%"), "%100\\%%");
    assert_eq!(like_pattern("a_b"), "%a\\_b%");
    // The backslash goes first: escaping it after the wildcards
    // would turn `\%` back into an escaped-backslash then a live `%`.
    assert_eq!(like_pattern("a\\b"), "%a\\\\b%");
    assert_eq!(like_pattern("a\\%b"), "%a\\\\\\%b%");
}
