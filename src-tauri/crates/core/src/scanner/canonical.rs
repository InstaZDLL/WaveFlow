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

/// The genres a tag's genre values name, in the order they first appear
/// (#729).
///
/// A tag can carry several values (an ID3v2.4 `TCON` with NUL-separated
/// values, repeated Vorbis `GENRE` comments, several MP4 `©gen`), and a
/// single value can still list several genres: taggers that only write one
/// field join them with `;` ("Rock; Jazz"), and some write the `\\` a
/// tag editor shows between multiple values into the file as text
/// ("Alternative\\Rock"). All are split: on `;`, NUL and `\`, which no
/// genre name contains. Not on `/` or `,`: they occur inside genre names
/// people actually use
/// ("Hip-Hop/Rap", "Folk, World, & Country"), and splitting on them would
/// invent genres. Each name is trimmed, and a name the library would file
/// under a genre already listed (same [`canonical_name`]) is dropped.
pub fn split_genres<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for value in values {
        for part in value.split([';', '\0', '\\']) {
            let name = part.trim();
            let key = canonical_name(name);
            if !key.is_empty() && seen.insert(key) {
                out.push(name.to_string());
            }
        }
    }
    out
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
mod tests;
