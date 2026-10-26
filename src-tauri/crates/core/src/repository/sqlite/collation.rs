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
mod tests;
