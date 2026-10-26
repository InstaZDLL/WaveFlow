//! Deciding what the user meant, without deciding how to ask for it.
//!
//! `track_fts` is tokenised with **trigram** (#579): every 3-character
//! window of every indexed field is a token, so any substring of three
//! characters or more can be found. That is what makes Chinese
//! searchable — `unicode61` turned an unbroken run of Han characters
//! into a single token, so only a query starting at the title's first
//! character ever matched, and typing correct characters from the middle
//! returned nothing at all.
//!
//! The cost of a substring index is a floor: **a term shorter than three
//! characters cannot be looked up in it**. That is not an exotic case —
//! most Chinese words are two characters, and plenty of artists are
//! short in Latin script too (`U2`, `M83`). A query carrying any such
//! term therefore has to be answered by scanning instead.
//!
//! Which of the two a query needs is a property of the query, not of the
//! database, so it is decided here. *How* each one is then expressed —
//! FTS5 phrase syntax, `LIKE` patterns, whatever a future backend wants
//! — belongs to that backend; for SQLite it lives in
//! `repository::sqlite::search`.

/// Shortest term a trigram index can answer. Below this it holds no
/// entry to look up — the result is nothing at all, not a worse match.
pub const TRIGRAM_MIN_CHARS: usize = 3;

/// How a search has to be answered.
///
/// Both variants carry the user's terms, cleaned and split, in the order
/// typed. Combining them is an AND: every term must appear somewhere in
/// the track's title, album or artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchPlan {
    /// Every term reaches [`TRIGRAM_MIN_CHARS`], so the substring index
    /// can answer the query — cheaply, and with a relevance ranking.
    Indexed(Vec<String>),
    /// At least one term is too short for the index, so the backend has
    /// to scan. Reserved for queries that would otherwise return
    /// nothing; an ordinary word never lands here.
    Scanned(Vec<String>),
}

impl SearchPlan {
    /// The terms, whichever route was chosen.
    pub fn terms(&self) -> &[String] {
        match self {
            SearchPlan::Indexed(t) | SearchPlan::Scanned(t) => t,
        }
    }
}

/// Decide how `raw` has to be answered, or `None` when there is nothing
/// to search for.
///
/// One short term is enough to send the whole query down the scanning
/// route: mixing the two would mean intersecting an index lookup with a
/// scan, which is more machinery than the case deserves — and the scan
/// has to happen either way.
pub fn plan_search(raw: &str) -> Option<SearchPlan> {
    // Double quotes are dropped rather than escaped: they delimit a
    // phrase in FTS5, and letting one through would end the phrase the
    // SQLite layer builds and leave the rest to be read as syntax.
    let cleaned = raw.replace('"', "");
    let terms: Vec<String> = cleaned.split_whitespace().map(str::to_owned).collect();
    if terms.is_empty() {
        return None;
    }

    if terms.iter().all(|t| t.chars().count() >= TRIGRAM_MIN_CHARS) {
        Some(SearchPlan::Indexed(terms))
    } else {
        Some(SearchPlan::Scanned(terms))
    }
}

#[cfg(test)]
mod tests;
