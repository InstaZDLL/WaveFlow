//! Fuzzy artist-name matching shared across metadata providers.
//!
//! Third-party catalogues (TheAudioDB, Deezer, …) return a *ranked* list
//! for a name query, but the top hit is rarely a byte-identical match: it
//! may carry a canonical backing-band suffix ("Bob Marley & The Wailers"),
//! or differ only by case, punctuation or diacritics ("Céline Dion" vs a
//! library tagged "Celine Dion"). Demanding exact equality silently drops
//! a perfectly good hit (issue #342), so every provider funnels its
//! candidate list through [`select_by_name`] instead.

/// Connectors that open a canonical "backing band" / collaboration
/// extension after normalization (`&` is already folded to "and"). Only a
/// suffix starting with one of these promotes a prefix into a match.
const CANONICAL_CONNECTORS: [&str; 5] = ["and", "with", "feat", "featuring", "ft"];

/// Select the entry whose name best matches `searched` (which MUST already
/// be [`normalize_name`]d). An exact normalized match wins wherever it
/// sits in the ranked list — so a homonym at index 0 can't shadow the real
/// entry — and failing that the first canonical-connector prefix match is
/// taken, preserving the provider's relevance order.
///
/// `name_of` extracts the raw (un-normalized) display name from each item;
/// items without a name are skipped.
pub fn select_by_name<T>(
    items: impl IntoIterator<Item = T>,
    searched: &str,
    name_of: impl Fn(&T) -> Option<&str>,
) -> Option<T> {
    if searched.is_empty() {
        return None;
    }
    let mut prefix_match: Option<T> = None;
    for item in items {
        let Some(candidate) = name_of(&item).map(normalize_name) else {
            continue;
        };
        if candidate == searched {
            return Some(item);
        }
        if prefix_match.is_none() && names_prefix_compatible(searched, &candidate) {
            prefix_match = Some(item);
        }
    }
    prefix_match
}

/// Normalise an artist name for fuzzy comparison: lowercase, fold common
/// Latin diacritics, expand `&` to "and", drop punctuation and collapse
/// whitespace to single spaces. "Boney M." and "Boney M" both become
/// "boney m"; "Bob Marley & The Wailers" becomes "bob marley and the
/// wailers"; "Beyoncé" becomes "beyonce". Non-Latin scripts (CJK, …) are
/// preserved verbatim minus punctuation.
pub fn normalize_name(name: &str) -> String {
    let lowered = name.to_lowercase().replace('&', " and ");
    let mut out = String::with_capacity(lowered.len());
    let mut pending_space = false;
    for ch in lowered.chars() {
        // Drop NFD combining marks (the accent half of a decomposed
        // character) so "Bjo\u{308}rk" folds to the same "bjork" as its
        // precomposed NFC form — without this they'd act as a boundary
        // and split the word. NOT a word boundary, so we `continue`
        // rather than fall into the punctuation branch below.
        if is_combining_mark(ch) {
            continue;
        }
        let folded = fold_diacritic(ch);
        if folded.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(folded);
        } else {
            // Punctuation or whitespace → a single word boundary.
            pending_space = true;
        }
    }
    out
}

/// True for Unicode nonspacing combining marks — the accent halves left
/// standing in an NFD-decomposed string. Covers the standard combining
/// blocks; dependency-free stand-in for the `Mn` general category so we
/// don't pull in `unicode-normalization` just to fold accents.
fn is_combining_mark(ch: char) -> bool {
    matches!(ch as u32,
        0x0300..=0x036F | // Combining Diacritical Marks
        0x1AB0..=0x1AFF | // Combining Diacritical Marks Extended
        0x1DC0..=0x1DFF | // Combining Diacritical Marks Supplement
        0x20D0..=0x20FF | // Combining Diacritical Marks for Symbols
        0xFE20..=0xFE2F,  // Combining Half Marks
    )
}

/// Map a lowercase accented Latin char to its base letter; pass anything
/// else through unchanged. Covers the accents common in Western artist
/// names without pulling in a Unicode-normalization dependency.
fn fold_diacritic(ch: char) -> char {
    match ch {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'ī' => 'i',
        'ñ' | 'ń' => 'n',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' | 'ō' => 'o',
        'ś' | 'š' => 's',
        'ú' | 'ù' | 'û' | 'ü' | 'ū' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// True when the two normalized names denote the same artist under a
/// canonical extension: one is the other followed by a recognized
/// connector — "bob marley" ↔ "bob marley and the wailers".
///
/// A bare word-boundary prefix is deliberately NOT enough. A
/// parenthetical disambiguator like "nirvana 60s band" (from "Nirvana
/// (60s band)") is a *different* artist that happens to share a leading
/// word, so the extension's first token must be an actual connector or we
/// reject it. Both inputs are single-space-joined with no leading/trailing
/// space, so the connector always sits right after the shared prefix.
fn names_prefix_compatible(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if short.is_empty() {
        return false;
    }
    let Some(rest) = long.strip_prefix(short) else {
        return false;
    };
    // Word-boundary split, then require a known connector as the first
    // token of the extension.
    let Some(tail) = rest.strip_prefix(' ') else {
        return false;
    };
    tail.split(' ')
        .next()
        .is_some_and(|token| CANONICAL_CONNECTORS.contains(&token))
}

#[cfg(test)]
mod tests {
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
        assert!(names_prefix_compatible("nirvana", "nirvana and the deep sea"));
    }

    #[test]
    fn empty_search_is_none() {
        assert_eq!(pick(&["Anything"], ""), None);
    }
}
