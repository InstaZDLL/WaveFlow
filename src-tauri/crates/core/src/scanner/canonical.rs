//! Pure-string helpers shared between [`super::extract`] and
//! [`super::upserts`]. Lives in its own module so the postgres-only
//! build (which skips `upserts`) can still consume `canonical_name`
//! from `extract`.

/// Normalize a title/name for dedup purposes: lowercase, strip punctuation
/// and collapse whitespace. Good enough to match "The Beatles" / "THE  BEATLES"
/// or "the beatles!" onto a single canonical key without pulling in a proper
/// Unicode normalization library.
pub fn canonical_name(s: &str) -> String {
    s.trim()
        .chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The romanised form of any Han characters in `s`, as one indexable
/// blob: the full syllables, a space, then their initials (#579).
///
/// `"中国人"` becomes `"zhongguoren zgr"`, so a single `LIKE '%…%'`
/// answers both of the things a pinyin search is asked for — the
/// syllables someone types instead of switching input method, and the
/// initials they type instead of the syllables. The space is what keeps
/// the two apart: the search planner splits terms on whitespace, so a
/// term can never contain one and no query can match across the
/// boundary.
///
/// **`None` when there is nothing to romanise**, which is the entire
/// library for most users: a Latin title has no Han characters, and
/// storing a copy of nothing for every row would be a column of empty
/// strings. Callers persist `""` to mean "computed, nothing to index",
/// so the backfill can tell that apart from "not computed yet" (`NULL`)
/// and terminate.
///
/// Both character sets are covered, because the mapping is by codepoint
/// — WaveFlow ships `zh-CN` and `zh-TW` and a traditional library has
/// the same problem as a simplified one.
///
/// Known and accepted: Japanese kanji romanise as if they were Chinese,
/// so a Japanese title can be reached by a pinyin query that has nothing
/// to do with it. That is a rare extra match, never a missing one —
/// this column only ever adds a way to find a track.
pub fn pinyin_blob(s: &str) -> Option<String> {
    use pinyin::ToPinyin;

    let mut full = String::new();
    let mut initials = String::new();
    // `to_pinyin` yields one entry per character and `None` for anything
    // that is not Han, so this skips Latin, punctuation and spacing
    // without having to recognise them.
    for syllable in s.to_pinyin().flatten() {
        full.push_str(syllable.plain());
        initials.push_str(syllable.first_letter());
    }
    if full.is_empty() {
        return None;
    }
    Some(format!("{full} {initials}"))
}

#[cfg(test)]
mod pinyin_tests {
    use super::pinyin_blob;

    #[test]
    fn han_gives_syllables_and_initials() {
        // The issue's own example: both forms have to reach the title.
        let blob = pinyin_blob("中国人").unwrap();
        assert_eq!(blob, "zhongguoren zgr");
        assert!(blob.contains("zhongguo"));
        assert!(blob.contains("zgr"));
    }

    #[test]
    fn a_latin_title_has_nothing_to_romanise() {
        // The common case, and the reason this is an Option: a column of
        // empty strings for every Western library would be paid for by
        // everyone and reachable by no one.
        assert_eq!(pinyin_blob("Dark Side of the Moon"), None);
        assert_eq!(pinyin_blob(""), None);
        assert_eq!(pinyin_blob("!!! (2004)"), None);
    }

    #[test]
    fn latin_around_han_is_skipped_not_romanised() {
        // A mixed title keeps only what pinyin can say something about.
        assert_eq!(pinyin_blob("周杰伦 - Album").unwrap(), "zhoujielun zjl");
    }

    #[test]
    fn traditional_characters_are_covered_too() {
        // zh-TW is a shipped locale; a traditional library has the same
        // defect as a simplified one.
        assert_eq!(pinyin_blob("愛").unwrap(), "ai a");
        assert_eq!(pinyin_blob("中國").unwrap(), "zhongguo zg");
    }

    #[test]
    fn the_two_halves_cannot_be_matched_across() {
        // The space is load-bearing: a term never contains one, so
        // "renz" must not be found in "zhongguoren zgr".
        let blob = pinyin_blob("中国人").unwrap();
        assert!(!blob.contains("renz"));
    }
}
