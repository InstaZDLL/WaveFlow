//! The `FOLD` collation: how a listing sorts a name.
//!
//! `COLLATE NOCASE` folds ASCII only, so a library sorted with it puts
//! "Émilie" after "Zoé" — `É` is U+00C9, past every ASCII letter — while
//! the A-Z rail files her under E. `FOLD` compares names the way a reader
//! does: case and accents set aside, with the same fold the remote
//! mirror's sort keys are computed with
//! ([`normalize_name`](crate::metadata::name_match::normalize_name)), so a
//! local "Björk" and a mirrored "bjork" land next to each other.
//!
//! SQLite knows nothing of it until a connection registers it, and a query
//! that names an unregistered collation fails to prepare. Every connection
//! that runs a listing must be opened through [`register`].

use std::cmp::Ordering;

use sqlx::sqlite::SqliteConnectOptions;

use crate::metadata::name_match::{fold_diacritic, is_combining_mark};

/// The name queries use: `ORDER BY ar.canonical_name COLLATE FOLD`.
pub const FOLD: &str = "FOLD";

/// Add the [`FOLD`] collation to every connection opened with `options`.
pub fn register(options: SqliteConnectOptions) -> SqliteConnectOptions {
    options.collation(FOLD, compare)
}

/// Accents and case set aside first; then case-insensitively with the
/// accents, so "Eric" comes before "Éric"; then byte for byte. The last
/// two only break ties, and they make it a total order: two different
/// strings never compare equal, so the sort is stable between queries.
pub fn compare(a: &str, b: &str) -> Ordering {
    folded(a)
        .cmp(folded(b))
        .then_with(|| lowered(a).cmp(lowered(b)))
        .then_with(|| a.cmp(b))
}

fn lowered(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().flat_map(char::to_lowercase)
}

/// Dropping combining marks folds a decomposed "e\u{301}" onto the same
/// "e" as its precomposed "é".
fn folded(s: &str) -> impl Iterator<Item = char> + '_ {
    lowered(s)
        .filter(|ch| !is_combining_mark(*ch))
        .map(fold_diacritic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        out.sort_by(|a, b| compare(a, b));
        out
    }

    #[test]
    fn accented_initials_sort_with_their_letter() {
        assert_eq!(
            sorted(&["Zoé", "Émilie", "eric", "Alain", "Ödland", "Oasis"]),
            ["Alain", "Émilie", "eric", "Oasis", "Ödland", "Zoé"]
        );
    }

    #[test]
    fn ties_resolve_to_a_total_order() {
        assert_eq!(compare("Eric", "Éric"), Ordering::Less);
        assert_eq!(compare("eric", "Eric"), Ordering::Greater);
        assert_eq!(compare("Éric", "Éric"), Ordering::Equal);
        // Decomposed and precomposed spellings differ only past the fold.
        assert_ne!(compare("E\u{301}ric", "Éric"), Ordering::Equal);
        assert_eq!(sorted(&["E\u{301}ric", "Erin"]), ["E\u{301}ric", "Erin"]);
    }

    #[test]
    fn non_latin_scripts_keep_their_code_point_order() {
        assert_eq!(
            sorted(&["東京", "Abba", "Ябеда"]),
            ["Abba", "Ябеда", "東京"]
        );
    }

    #[tokio::test]
    async fn sqlite_sorts_with_the_registered_collation() {
        use sqlx::sqlite::SqlitePoolOptions;
        use std::str::FromStr;

        let options = register(SqliteConnectOptions::from_str(":memory:").unwrap());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM (SELECT 'Zoé' AS name UNION ALL SELECT 'Émilie'
                               UNION ALL SELECT 'Alain')
              ORDER BY name COLLATE FOLD",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(names, ["Alain", "Émilie", "Zoé"]);
    }
}
