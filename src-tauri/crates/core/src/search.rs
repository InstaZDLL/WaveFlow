//! Turning what the user typed into something SQLite can answer.
//!
//! `track_fts` is tokenised with **trigram** (#579): every 3-character
//! window of every indexed field is a token, so any substring of three
//! characters or more is a phrase match. That is what makes Chinese
//! searchable — `unicode61` turned an unbroken run of Han characters into
//! a single token, so only a query starting at the title's first
//! character ever matched, and typing correct characters from the middle
//! returned nothing at all.
//!
//! The cost of trigram is a floor: **a term shorter than three characters
//! cannot be a phrase**, and `MATCH` simply finds nothing for it. That is
//! not an exotic case — most Chinese words are two characters, and plenty
//! of artists are short in Latin script too (`U2`, `M83`). So a query
//! carrying any such term takes a different route: `LIKE '%term%'` over
//! the same three fields.
//!
//! Measured on 50 000 tracks, which is what settles the shape:
//!
//! | Route | Cost |
//! | --- | --- |
//! | `MATCH`, terms of 3+ characters | ~0.2 ms — the index answers it |
//! | `LIKE`, a 2-character term | ~26 ms — a full scan, no index can help |
//!
//! So the fallback is reserved for exactly the queries that would
//! otherwise return nothing, and never taken by an ordinary word.

/// Shortest term the trigram index can answer. Below this, `MATCH`
/// matches nothing at all — not "less well", nothing.
pub const TRIGRAM_MIN_CHARS: usize = 3;

/// The character `LIKE … ESCAPE` uses below. Backslash, so a pattern
/// reads the way a programmer expects.
pub const LIKE_ESCAPE: char = '\\';

/// The SQL one `LIKE` term expands to, over the three searched columns.
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
     OR ar.name  LIKE ? ESCAPE '\\')";

/// How many values [`LIKE_TERM_CLAUSE`] expects, one per column.
pub const LIKE_TERM_BINDS: usize = 3;

/// How a search reaches SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchPlan {
    /// Every term is long enough for the index: one FTS5 expression,
    /// `"term" "term"` — quoted so each is a phrase (a substring match)
    /// and so nothing the user types is read as an FTS5 operator.
    /// Space between phrases is AND.
    Match(String),
    /// At least one term is shorter than a trigram. Each entry is a
    /// ready-to-bind `%pattern%`, already escaped for [`LIKE_ESCAPE`];
    /// the caller ANDs them across `title` / `album_title` /
    /// `artist_name`.
    Like(Vec<String>),
}

/// Decide how to run `raw`, or `None` when there is nothing to search.
///
/// One short term is enough to send the whole query down the `LIKE`
/// route: mixing the two would mean intersecting an FTS rowid set with a
/// scan, which is more machinery than the case deserves — and the scan
/// has to happen either way.
pub fn plan_search(raw: &str) -> Option<SearchPlan> {
    // Double quotes are stripped rather than escaped: they are the FTS5
    // phrase delimiter, and this function adds its own.
    let cleaned = raw.replace('"', "");
    let terms: Vec<&str> = cleaned.split_whitespace().collect();
    if terms.is_empty() {
        return None;
    }

    if terms.iter().all(|t| t.chars().count() >= TRIGRAM_MIN_CHARS) {
        let expr = terms
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" ");
        return Some(SearchPlan::Match(expr));
    }

    Some(SearchPlan::Like(
        terms.iter().map(|t| like_pattern(t)).collect(),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(q: &str) -> String {
        match plan_search(q) {
            Some(SearchPlan::Match(e)) => e,
            other => panic!("expected a MATCH plan for {q:?}, got {other:?}"),
        }
    }

    fn liked(q: &str) -> Vec<String> {
        match plan_search(q) {
            Some(SearchPlan::Like(p)) => p,
            other => panic!("expected a LIKE plan for {q:?}, got {other:?}"),
        }
    }

    #[test]
    fn nothing_to_search_is_not_a_plan() {
        assert_eq!(plan_search(""), None);
        assert_eq!(plan_search("   "), None);
        // Quotes are stripped before the split, so a query of nothing but
        // quotes has no terms left.
        assert_eq!(plan_search("\"\""), None);
    }

    #[test]
    fn ordinary_words_go_to_the_index_as_phrases() {
        assert_eq!(matched("moon"), "\"moon\"");
        assert_eq!(matched("dark side"), "\"dark\" \"side\"");
    }

    /// The defect this exists for: three Han characters are three
    /// characters, not nine bytes, and they belong on the fast route.
    #[test]
    fn han_characters_are_counted_as_characters() {
        assert_eq!(matched("人民音"), "\"人民音\"");
        // Two of them cannot be a trigram, so they take the fallback —
        // which is the common case in Chinese, not an edge one.
        assert_eq!(liked("人民"), vec!["%人民%"]);
    }

    /// A short term anywhere sends the whole query down the fallback,
    /// including the terms that would have been fine on their own.
    #[test]
    fn one_short_term_decides_for_the_whole_query() {
        assert_eq!(liked("u2 sunday"), vec!["%u2%", "%sunday%"]);
    }

    /// `U2` and `M83` matched before this change (a prefix on a whole
    /// token) and must keep matching after it, or trigram would be a
    /// regression for short Latin names.
    #[test]
    fn short_latin_names_still_reach_a_route() {
        assert_eq!(liked("u2"), vec!["%u2%"]);
        assert_eq!(liked("ok"), vec!["%ok%"]);
    }

    /// FTS5 reads bare `*`, `^`, `:`, `AND`, `NOT` as syntax. Quoting
    /// every term keeps a query of `AND` a search for the word.
    #[test]
    fn fts_operators_are_neutralised_by_quoting() {
        assert_eq!(matched("AND"), "\"AND\"");
        assert_eq!(matched("foo* bar"), "\"foo*\" \"bar\"");
        // An embedded quote would close the phrase and leave the rest as
        // syntax, so it is removed rather than escaped.
        assert_eq!(matched("foo\"bar"), "\"foobar\"");
    }

    /// The clause and its bind count must agree — a mismatch shifts
    /// every following positional parameter without any error.
    #[test]
    fn the_like_clause_has_one_placeholder_per_declared_bind() {
        assert_eq!(LIKE_TERM_CLAUSE.matches('?').count(), LIKE_TERM_BINDS);
        // And the escape character it names is the one `like_pattern`
        // escapes with.
        assert!(LIKE_TERM_CLAUSE.contains(&format!("ESCAPE '{LIKE_ESCAPE}'")));
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
