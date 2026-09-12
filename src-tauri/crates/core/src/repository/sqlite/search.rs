//! Turning a [`SearchPlan`] into SQLite.
//!
//! The decision of *which* route a query needs is portable and lives in
//! [`crate::search`]. Everything here is SQLite-flavoured — FTS5 phrase
//! syntax, `LIKE` wildcards, the column aliases both callers join under
//! — so it sits behind the `sqlite` feature rather than in the
//! storage-agnostic layer, where a future backend would have to parse it
//! back out of a string it never wanted.

use crate::search::SearchPlan;

/// The character `LIKE … ESCAPE` uses below. Backslash, so a pattern
/// reads the way a programmer expects.
pub const LIKE_ESCAPE: char = '\\';

/// The SQL one scanned term expands to, over the three searched columns.
///
/// Both search paths build the same clause, so it lives here as one
/// string rather than being written twice — and [`LIKE_TERM_BINDS`] sits
/// next to it because the two must agree: the placeholders are
/// positional, and binding two values for three `?` shifts every
/// following parameter silently.
///
/// The aliases are the ones both callers already join under: `t` for
/// `track`, `al` for `album`, `ar` for `artist`.
pub const LIKE_TERM_CLAUSE: &str = "(t.title LIKE ? ESCAPE '\\' \
     OR al.title LIKE ? ESCAPE '\\' \
     OR ar.name  LIKE ? ESCAPE '\\' \
     OR t.pinyin LIKE ? ESCAPE '\\' \
     OR al.pinyin LIKE ? ESCAPE '\\' \
     OR ar.pinyin LIKE ? ESCAPE '\\')";

/// How many values [`LIKE_TERM_CLAUSE`] expects, one per column.
///
/// Six since #579: the three texts, and the three pinyin blobs beside
/// them. The blobs matter most on this route — a term below the trigram
/// floor is exactly the shape a two-character word's initials take
/// (`zg`), and the index holds nothing to look it up in.
pub const LIKE_TERM_BINDS: usize = 6;

/// The FTS5 expression for an indexed plan.
///
/// Each term becomes a quoted phrase, which is what makes it a substring
/// match on a trigram index — and which also neutralises everything
/// FTS5 would otherwise read as syntax (`*`, `^`, `:`, `AND`, `NOT`).
/// A space between phrases is AND.
pub fn fts5_expression(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wrap a term as `%term%`, escaping what `LIKE` would otherwise read as
/// a wildcard. Without this a query of `%` matches the whole library and
/// `_` matches any character — surprising, and a needless full scan.
///
/// The backslash is escaped first, or it would double-escape the
/// wildcards added after it.
pub fn like_pattern(term: &str) -> String {
    let escaped = term
        .replace(LIKE_ESCAPE, &format!("{LIKE_ESCAPE}{LIKE_ESCAPE}"))
        .replace('%', &format!("{LIKE_ESCAPE}%"))
        .replace('_', &format!("{LIKE_ESCAPE}_"));
    format!("%{escaped}%")
}

/// Every `%term%` pattern a scanned plan needs, in bind order.
pub fn like_patterns(plan: &SearchPlan) -> Vec<String> {
    plan.terms().iter().map(|t| like_pattern(t)).collect()
}

#[cfg(test)]
mod tests {
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
}
