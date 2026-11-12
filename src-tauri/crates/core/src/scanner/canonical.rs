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

/// The artist ids a file keeps when an edit rewrites its artist credit from
/// `old_credit` to `new_credit` and names no ids of its own (#1076).
///
/// A scan pairs a file's ids with its names by position, and only when
/// there are as many of each ([`artist_credits`]). An id left behind by an
/// edit that changed the names would follow the new ones: the track
/// renamed "Aimer" would go straight back to "LISA" on its next scan,
/// found by her id. So:
///
/// - `None`: the ids stay as they are. The file has none, the new names are
///   the old ones up to case, punctuation and spacing (a spelling fix keeps
///   the identity), or ids that never paired still cannot pair.
/// - `Some(ids)`: write these instead. The old ids reordered when every new
///   name is an old one in a new order; empty, to remove them, as soon as
///   one name is new, since an id list cannot leave one name without its
///   id and keep the others paired.
///
/// Two old names that differ by case only ("LiSA; LISA", two ids, #1081)
/// are two artists: a new name is the one it matches letter for letter; the
/// namesakes no name matches that way pair, in order, with the names of
/// their spelling left, when there are as many (the properties dialog shows
/// the library's spelling, which may not be the file's). A spelling that
/// still fits several of them ("Lisa" alone) names no one, so the ids go.
pub fn reconcile_artist_mbids(
    old_credit: &str,
    old_ids: &[String],
    new_credit: &str,
) -> Option<Vec<String>> {
    if old_ids.is_empty() {
        return None;
    }
    let mut old = artist_credits(old_credit, old_ids);
    if old.iter().any(|(_, id)| id.is_none()) {
        // Not paired as written; the names a scan files together may still
        // be as many as the ids ("Beta; BETA" and one id).
        let names = split_artist_name(old_credit);
        if names.len() != old_ids.len() {
            // Never paired. They would as soon as the counts met, as a scan
            // counts them: the names as written or as filed together.
            let pairs = credit_parts(new_credit).count() == old_ids.len()
                || split_artist_name(new_credit).len() == old_ids.len();
            return pairs.then(Vec::new);
        }
        old = names
            .into_iter()
            .zip(old_ids.iter().cloned().map(Some))
            .collect();
    }
    // Every name as typed, so "LiSA; LISA" keeps both of its artists.
    let new: Vec<(&str, String)> = credit_parts(new_credit)
        .map(|name| (name, canonical_name(name)))
        .collect();
    let old_keys: Vec<String> = old.iter().map(|(name, _)| canonical_name(name)).collect();
    let group =
        |key: &str| -> Vec<usize> { (0..old.len()).filter(|&i| old_keys[i] == key).collect() };
    let mut found: Vec<Option<usize>> = vec![None; new.len()];
    let mut taken = vec![false; old.len()];
    // A name only one old artist answers to is that artist, whatever its
    // spelling; among namesakes, the one spelled the same, when the new
    // credit spells it as many times as the old one ("Lisa" alone over two
    // artists both spelled "Lisa" is neither).
    for (at, (name, key)) in new.iter().enumerate() {
        match group(key).as_slice() {
            [] => return Some(Vec::new()),
            [only] => found[at] = Some(*only),
            several => {
                let exact_old = several.iter().filter(|&&i| old[i].0 == *name).count();
                let exact_new = new.iter().filter(|(n, _)| n == name).count();
                if exact_old != exact_new {
                    continue;
                }
                if let Some(&exact) = several.iter().find(|&&i| !taken[i] && old[i].0 == *name) {
                    taken[exact] = true;
                    found[at] = Some(exact);
                }
            }
        }
    }
    // The namesakes left, in credit order, when as many names are left for
    // them: the dialog shows the library's spelling, which need not be the
    // file's ("Lisa; LISA" over a file's "LiSA; LISA").
    for (at, (_, key)) in new.iter().enumerate() {
        if found[at].is_some() {
            continue;
        }
        let names_left: Vec<usize> = (at..new.len())
            .filter(|&j| found[j].is_none() && new[j].1 == *key)
            .collect();
        let old_left: Vec<usize> = group(key).into_iter().filter(|&i| !taken[i]).collect();
        if names_left.len() != old_left.len() {
            // A spelling that fits several of them names no one of them.
            return Some(Vec::new());
        }
        for (j, i) in names_left.into_iter().zip(old_left) {
            taken[i] = true;
            found[j] = Some(i);
        }
    }
    let mut ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for ((_, key), i) in new.iter().zip(found) {
        let id = i.and_then(|i| old[i].1.clone()).unwrap_or_default();
        if seen.insert((key.clone(), id.clone())) {
            ids.push(id);
        }
    }
    (ids != old_ids).then_some(ids)
}

/// The names a credit lists, trimmed and non-empty, in order and as written:
/// a name listed twice stays twice.
fn credit_parts(raw: &str) -> impl Iterator<Item = &str> {
    raw.split("; ").map(str::trim).filter(|s| !s.is_empty())
}

/// The artists a credit names, each with the MusicBrainz id the file gives
/// it (#1076).
///
/// `mbids` pair with the names by position, and only when there are as many
/// of them as names: a tag with fewer ids than artists does not say whose
/// they are, and a guess would hand an artist someone else's identity. A
/// name the credit lists twice (same canonical form, same id) is one artist,
/// as in [`split_artist_name`], which this is when no id pairs. The same name
/// under two ids is two (#1081).
pub fn artist_credits(raw: &str, mbids: &[String]) -> Vec<(String, Option<String>)> {
    let parts: Vec<&str> = credit_parts(raw).collect();
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

/// Every name a credit lists, trimmed and non-empty, as written: a name
/// listed twice, or two that differ by case only, stay as they are.
pub fn artist_credit_parts(raw: &str) -> Vec<String> {
    credit_parts(raw).map(str::to_owned).collect()
}

/// The names [`artist_credits`] gives, without their ids: how many artists a
/// credit names, and which, when its file's ids tell namesakes apart.
pub fn artist_credit_names(raw: &str, mbids: &[String]) -> Vec<String> {
    artist_credits(raw, mbids)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
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
    artist_credit_with_ids(artists, artist, &[]).0
}

/// [`artist_credit`] for a file that also carries MusicBrainz artist ids
/// (`mbids`, see [`artist_mbids`]), and the ids that go with the credit.
///
/// A name listed twice is one artist unless its ids differ: when the ids
/// pair with the names as written ("LiSA; LISA" and two ids), the names are
/// deduplicated on `(canonical name, id)` and the ids come back aligned with
/// the names that remain, so [`artist_credits`] pairs them again. Otherwise
/// the names are deduplicated by canonical name alone and the ids are
/// returned as they came (#1081).
pub fn artist_credit_with_ids<'a>(
    artists: impl IntoIterator<Item = &'a str>,
    artist: impl IntoIterator<Item = &'a str>,
    mbids: &[String],
) -> (Option<String>, Vec<String>) {
    fn names<'a>(
        values: impl IntoIterator<Item = &'a str>,
        mbids: &[String],
    ) -> (Vec<String>, Vec<String>) {
        let joined: Vec<&str> = values.into_iter().flat_map(|v| v.split('\0')).collect();
        let joined = joined.join("; ");
        let parts = credit_parts(&joined).count();
        if !mbids.is_empty() && mbids.len() == parts {
            let (names, ids): (Vec<_>, Vec<_>) = artist_credits(&joined, mbids)
                .into_iter()
                .map(|(name, id)| (name, id.unwrap_or_default()))
                .unzip();
            (names, ids)
        } else {
            (split_artist_name(&joined), mbids.to_vec())
        }
    }
    let listed = names(artists, mbids);
    let credited = names(artist, mbids);
    let chosen = if listed.0.len() > credited.0.len() {
        listed
    } else {
        credited
    };
    if chosen.0.is_empty() {
        (None, Vec::new())
    } else {
        (Some(chosen.0.join("; ")), chosen.1)
    }
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
