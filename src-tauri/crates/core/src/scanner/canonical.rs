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

/// Split a raw artist string into individual names. Only `"; "` is
/// honoured as a separator — the convention used by MusicBrainz Picard,
/// foobar2000, Beets and Mp3Tag for multi-value artist fields. We
/// deliberately do **not** split on `", "` even though plenty of
/// ad-hoc taggers use it, because a comma can be part of the name
/// itself (`"Tyler, The Creator"`, `"Earth, Wind & Fire"`,
/// `"Crosby, Stills, Nash & Young"`); the earlier comma split
/// silently fragmented those into multiple artists.
///
/// Libraries that stored multi-artist values comma-joined will see
/// every track listed under the combined-name artist; the user can
/// re-tag with `; ` (the round-trip is documented in CLAUDE.md and
/// `docs/features/library.md`) to opt back in to per-artist linking.
///
/// Returns the trimmed, non-empty names in the order they appeared —
/// the first entry is treated as the primary artist by the caller. A name
/// the library would file under an artist already listed (same
/// [`canonical_name`]) is dropped: `track_artist` holds an artist once per
/// role, so a second credit would abort the scan's transaction.
pub fn split_artist_name(raw: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.split("; ")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(canonical_name(s)))
        .map(|s| s.to_string())
        .collect()
}

/// The MusicBrainz artist ids a tag holds, lowercased, in the order the tag
/// lists them.
///
/// The tag is one value per artist, but taggers join them differently
/// (several values, `"; "`, `/`, a NUL), so the values are not split on a
/// separator: every run of hex digits and dashes shaped like an id is one,
/// whatever sits between them. Anything else in the tag is dropped.
///
/// A value shaped like an id (dashed groups of hex digits, or a run of 32
/// hex digits or more) that is not one makes the whole list empty: dropping it would shift the ids after it
/// onto the wrong names, and [`artist_credits`] pairs by position.
pub fn artist_mbids<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    fn is_mbid(token: &str) -> bool {
        let groups: Vec<&str> = token.split('-').collect();
        groups.len() == 5
            && groups
                .iter()
                .zip([8, 4, 4, 4, 12])
                .all(|(g, len)| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()))
    }
    let tokens: Vec<&str> = values
        .into_iter()
        .flat_map(|v| v.split(|c: char| !(c.is_ascii_hexdigit() || c == '-')))
        .collect();
    if tokens
        .iter()
        .any(|t| (t.len() >= 32 || (t.len() >= 8 && t.contains('-'))) && !is_mbid(t))
    {
        return Vec::new();
    }
    tokens
        .into_iter()
        .filter(|t| is_mbid(t))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The MusicBrainz id an Album Artist credit carries, when it names one
/// artist and the tag one id.
///
/// The names are counted as written, before any deduplication: "LiSA;
/// LISA" is two artists even though [`split_artist_name`] files them under
/// one key, and a single id would belong to only one of them.
pub fn album_artist_mbid<'a>(
    album_artist: &str,
    values: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let names = album_artist
        .split("; ")
        .filter(|s| !s.trim().is_empty())
        .count();
    if names != 1 {
        return None;
    }
    // The same id written twice is still one.
    let mut ids = artist_mbids(values);
    ids.dedup();
    (ids.len() == 1).then(|| ids.remove(0))
}

/// The artists a credit names, each with the MusicBrainz id the file gives
/// it (#1076).
///
/// `mbids` pair with the names by position, and only when there are as many
/// of them as names: a tag with fewer ids than artists does not say whose
/// they are, and a guess would hand an artist someone else's identity. A
/// name the credit lists twice (same canonical form, same id) is one artist,
/// as in [`split_artist_name`], which this is when no id pairs.
pub fn artist_credits(raw: &str, mbids: &[String]) -> Vec<(String, Option<String>)> {
    let parts: Vec<&str> = raw
        .split("; ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let paired = !mbids.is_empty() && mbids.len() == parts.len();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        let mbid = paired.then(|| mbids[i].clone());
        if seen.insert((canonical_name(part), mbid.clone())) {
            out.push((part.to_string(), mbid));
        }
    }
    out
}

/// The credit a file gives a track, as the `"; "`-joined string the rest of
/// the scanner works from, or `None` when it names nobody.
///
/// `artists` is what the `ARTISTS` tag holds, the list of names where
/// `ARTIST` may be the credit as printed (`"Beta feat. Alpha"`); `artist` is
/// every value of the artist tag: repeated Vorbis comments, a multi-value
/// `TPE1`, several `©ART`. A value may itself be joined, on `"; "` or on the
/// NUL of an ID3v2 frame nobody split.
///
/// `ARTISTS` wins when it says more, that is when it lists more names than
/// `ARTIST` does. One name beside one name adds nothing, and it is what a
/// tagger that rewrote `ARTIST` and knows nothing of this tag leaves behind.
pub fn artist_credit<'a>(
    artists: impl IntoIterator<Item = &'a str>,
    artist: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    fn names<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let joined: Vec<&str> = values.into_iter().flat_map(|v| v.split('\0')).collect();
        split_artist_name(&joined.join("; "))
    }
    let listed = names(artists);
    let credited = names(artist);
    let chosen = if listed.len() > credited.len() {
        listed
    } else {
        credited
    };
    (!chosen.is_empty()).then(|| chosen.join("; "))
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

#[cfg(test)]
mod credit_tests {
    use super::*;

    #[test]
    fn a_name_the_library_would_file_together_is_one_name() {
        assert_eq!(split_artist_name("Beta; BETA; beta!"), ["Beta"]);
        assert_eq!(split_artist_name("Beta; Alpha"), ["Beta", "Alpha"]);
    }

    #[test]
    fn a_comma_is_part_of_a_name() {
        assert_eq!(
            split_artist_name("Tyler, The Creator; Beta"),
            ["Tyler, The Creator", "Beta"]
        );
    }

    #[test]
    fn every_value_of_the_artist_tag_counts() {
        assert_eq!(
            artist_credit([], ["Beta", "Alpha; Gamma", "Delta\0Epsilon"]).as_deref(),
            Some("Beta; Alpha; Gamma; Delta; Epsilon")
        );
    }

    #[test]
    fn the_artists_tag_wins_only_when_it_says_more() {
        assert_eq!(
            artist_credit(["Beta", "Alpha"], ["Beta feat. Alpha"]).as_deref(),
            Some("Beta; Alpha")
        );
        assert_eq!(
            artist_credit(["Beta\0Alpha"], ["Beta feat. Alpha"]).as_deref(),
            Some("Beta; Alpha")
        );
        assert_eq!(artist_credit(["Old"], ["New"]).as_deref(), Some("New"));
        assert_eq!(artist_credit(["Only"], []).as_deref(), Some("Only"));
        assert_eq!(
            artist_credit(["Beta; Alpha"], ["Beta", "Alpha", "Gamma"]).as_deref(),
            Some("Beta; Alpha; Gamma"),
            "two names do not outrank three"
        );
    }

    #[test]
    fn nobody_is_no_credit() {
        assert_eq!(artist_credit([], []), None);
        assert_eq!(artist_credit([" "], [""]), None);
    }
}
