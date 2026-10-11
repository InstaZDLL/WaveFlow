//! The `FOLD` collation: how a listing sorts a name.
//!
//! `COLLATE NOCASE` folds ASCII only, so a library sorted with it puts
//! "Émilie" after "Zoé" — `É` is U+00C9, past every ASCII letter — while
//! the A-Z rail files her under E. `FOLD` compares names the way a reader
//! does: case and accents set aside, so a local "Björk" and a mirrored
//! "bjork" (the mirror's sort keys go through
//! [`normalize_name`](crate::metadata::name_match::normalize_name)) land
//! next to each other.
//!
//! The fold decomposes every letter (NFD) and drops the marks, so any
//! decomposable accent — "Ř", "Ő", "Ș", Vietnamese — sorts with its base
//! letter, not only the ones `normalize_name`'s table lists. That table
//! still runs after it for the letters NFD leaves whole ("ø"), and
//! [`fold_stroke`] covers the rest of them.
//!
//! [`NAME`] is the same fold for the names of artists and albums, with a
//! leading article set aside when the user keeps that on: "The Beatles"
//! sorts under B, "Les Rita Mitsouko" under R. Titles of tracks, genres
//! and folders keep `FOLD`.
//!
//! SQLite knows nothing of either until a connection registers them, and a
//! query that names an unregistered collation fails to prepare. Every
//! connection that runs a listing must be opened through [`register`].

use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use sqlx::sqlite::SqliteConnectOptions;

use unicode_normalization::UnicodeNormalization;

use crate::metadata::name_match::{fold_diacritic, is_combining_mark};

/// The name queries use for titles, genres and folders: `ORDER BY t.title COLLATE FOLD`.
pub const FOLD: &str = "FOLD";

/// The name artist and album listings use: `ORDER BY sort_name COLLATE NAME`.
pub const NAME: &str = "NAME";

/// Whether [`NAME`] sets a leading article aside. Process-wide, like the
/// preference it mirrors (`app_setting['library.sort_ignore_articles']`);
/// read on every comparison, so a change applies to the next listing.
/// No index is built on the collation, so nothing stored depends on it.
static IGNORE_ARTICLES: AtomicBool = AtomicBool::new(true);

pub fn set_ignore_articles(enabled: bool) {
    IGNORE_ARTICLES.store(enabled, AtomicOrdering::Relaxed);
}

pub fn ignores_articles() -> bool {
    IGNORE_ARTICLES.load(AtomicOrdering::Relaxed)
}

/// Add the [`FOLD`] and [`NAME`] collations to every connection opened
/// with `options`.
pub fn register(options: SqliteConnectOptions) -> SqliteConnectOptions {
    options
        .collation(FOLD, compare)
        .collation(NAME, |a, b| compare_names(a, b, ignores_articles()))
}

/// Articles a name may open with, followed by a space. The definite
/// articles of the languages the app ships in that write them as a word,
/// plus English's indefinite ones, which are as common at the head of a
/// band name. Kept in step with `ARTICLES` in `src/lib/sortName.ts`, which
/// files a name under the same letter of the A–Z rail.
///
/// The listings sort on `canonical_name` / `canonical_title`, which turn
/// punctuation into spaces: "L'Impératrice" is stored as `l impératrice`,
/// so the elided `l` is a word here like the others. The same rule has a
/// cost the rail shares: "A-ha", stored `a ha`, files under H. Dutch
/// `de` is left out on purpose: it is as often a particle of a name ("De
/// La Soul", "De Palmas") as an article, and `het` is enough there.
const ARTICLES: &[&str] = &[
    "the", "a", "an", "le", "la", "les", "l", "der", "die", "das", "het", "el", "los", "las", "il",
    "lo", "gli",
];

/// Elided forms, written against the next word: "L'Impératrice", for the
/// columns that sort on a display name rather than a canonical key
/// (`sort_artist` falls back to the raw album artist).
const ELIDED: &[&str] = &["l'", "l\u{2019}"];

/// `name` without a leading article, or `name` itself. One article at
/// most ("The The" sorts as "The"); a name that is only an article is kept
/// whole, and so is one whose article runs into the next word ("Theory").
pub fn strip_article(name: &str) -> &str {
    let trimmed = name.trim_start();
    for article in ARTICLES {
        let len = article.len();
        if trimmed.len() > len + 1
            && trimmed.is_char_boundary(len)
            && trimmed[..len].eq_ignore_ascii_case(article)
            && trimmed[len..].starts_with(' ')
        {
            let rest = trimmed[len..].trim_start();
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    for elided in ELIDED {
        let len = elided.len();
        if trimmed.len() > len
            && trimmed.is_char_boundary(len)
            && trimmed[..len].to_lowercase() == *elided
        {
            let rest = &trimmed[len..];
            if !rest.trim().is_empty() {
                return rest;
            }
        }
    }
    trimmed
}

/// [`compare`] on the names without their leading article, when
/// `ignore_articles`; the full names then break the tie, so "The Kills"
/// and "Kills" stay a total order.
pub fn compare_names(a: &str, b: &str, ignore_articles: bool) -> Ordering {
    if !ignore_articles {
        return compare(a, b);
    }
    compare(strip_article(a), strip_article(b)).then_with(|| compare(a, b))
}

/// Accents and case set aside first; then case-insensitively with the
/// accents, so "Eric" comes before "Éric"; then byte for byte. The last
/// two only break ties, and they make it a total order: two different
/// strings never compare equal, so the sort is stable between queries.
pub fn compare(a: &str, b: &str) -> Ordering {
    // Most names are ASCII, and for them the fold is only the lowercasing:
    // no decomposition to pay for, once per comparison of every sort.
    let primary = if a.is_ascii() && b.is_ascii() {
        ascii_lowered(a).cmp(ascii_lowered(b))
    } else {
        folded(a).cmp(folded(b))
    };
    primary
        .then_with(|| lowered(a).cmp(lowered(b)))
        .then_with(|| a.cmp(b))
}

fn ascii_lowered(s: &str) -> impl Iterator<Item = u8> + '_ {
    s.bytes().map(|byte| byte.to_ascii_lowercase())
}

fn lowered(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().flat_map(char::to_lowercase)
}

/// Decomposed, then stripped of its marks: "é" and "e\u{301}" both fold
/// to "e". Lazy, like [`lowered`]: a comparison allocates nothing.
fn folded(s: &str) -> impl Iterator<Item = char> + '_ {
    s.nfd()
        .filter(|ch| !is_combining_mark(*ch))
        .flat_map(char::to_lowercase)
        .map(fold_diacritic)
        .map(fold_stroke)
}

/// Latin letters whose accent is part of the glyph, so NFD has nothing to
/// strip and `fold_diacritic`'s table does not list them.
fn fold_stroke(ch: char) -> char {
    match ch {
        'ł' => 'l',
        'đ' => 'd',
        'ħ' => 'h',
        'ı' => 'i',
        'ŧ' => 't',
        other => other,
    }
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

    /// Letters `normalize_name`'s table does not list still fold: through
    /// NFD for the decomposable ones, through `fold_stroke` for the rest.
    #[test]
    fn accents_outside_the_fold_table_sort_with_their_letter() {
        assert_eq!(
            sorted(&["Zeta", "Řeka", "Rosa", "Ştefan", "Łódź", "Lima", "Sara"]),
            ["Lima", "Łódź", "Řeka", "Rosa", "Sara", "Ştefan", "Zeta"]
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

    fn sorted_names(names: &[&str], ignore_articles: bool) -> Vec<String> {
        let mut out: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        out.sort_by(|a, b| compare_names(a, b, ignore_articles));
        out
    }

    #[test]
    fn a_leading_article_is_set_aside() {
        assert_eq!(
            sorted_names(
                &[
                    "The Beatles",
                    "Air",
                    "Les Rita Mitsouko",
                    "L'Impératrice",
                    "Blur",
                    "Muse"
                ],
                true
            ),
            [
                "Air",
                "The Beatles",
                "Blur",
                "L'Impératrice",
                "Muse",
                "Les Rita Mitsouko"
            ]
        );
    }

    #[test]
    fn off_it_is_the_plain_fold() {
        assert_eq!(
            sorted_names(&["The Beatles", "Blur", "Air"], false),
            ["Air", "Blur", "The Beatles"]
        );
    }

    #[test]
    fn what_is_not_an_article_stays() {
        // A word that only starts like one, a name that is only one, a
        // hyphen rather than a space, and the curly apostrophe.
        assert_eq!(strip_article("Theory of a Deadman"), "Theory of a Deadman");
        assert_eq!(strip_article("The"), "The");
        assert_eq!(strip_article("The The"), "The");
        assert_eq!(strip_article("A-ha"), "A-ha");
        assert_eq!(strip_article("Lana Del Rey"), "Lana Del Rey");
        assert_eq!(strip_article("L\u{2019}Impératrice"), "Impératrice");
        assert_eq!(strip_article("  the  Kills"), "Kills");
        assert_eq!(strip_article("L'"), "L'");
    }

    /// What the listings actually sort: the scanner's canonical keys,
    /// lowercased with punctuation turned into spaces.
    #[test]
    fn canonical_keys_lose_their_article_too() {
        assert_eq!(strip_article("l impératrice"), "impératrice");
        assert_eq!(strip_article("the beatles"), "beatles");
        assert_eq!(strip_article("lana del rey"), "lana del rey");
        assert_eq!(strip_article("het goede doel"), "goede doel");
        assert_eq!(strip_article("de la soul"), "de la soul");
        assert_eq!(
            sorted_names(&["the beatles", "l impératrice", "blur", "muse"], true),
            ["the beatles", "blur", "l impératrice", "muse"]
        );
    }

    #[test]
    fn the_full_name_breaks_the_tie() {
        assert_eq!(compare_names("Kills", "The Kills", true), Ordering::Less);
        assert_eq!(
            compare_names("The Kills", "The Kills", true),
            Ordering::Equal
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

        // NAME is registered beside it; the flag's default sets the article
        // aside. The test does not touch the flag: other tests share it.
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM (SELECT 'The Beatles' AS name UNION ALL SELECT 'Blur'
                               UNION ALL SELECT 'Air')
              ORDER BY name COLLATE NAME",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(names, ["Air", "The Beatles", "Blur"]);
    }
}
