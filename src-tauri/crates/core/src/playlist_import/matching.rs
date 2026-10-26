//! Which library track an imported entry (or a placeholder) is.
//!
//! Strongest key first, each one tried only when the one before found
//! nothing:
//!
//! 1. **The path an M3U named** — its canonical form, then its file name
//!    alone, as the M3U importer always did: a playlist written before the
//!    library was reorganised still finds its files.
//! 2. **ISRC** — exact, and the same recording whatever the tags say. When
//!    several tracks carry it (a single and its album), the closest length
//!    wins.
//! 3. **Title + artist + length** — the title compared through
//!    [`normalize_name`], the artist through the shared
//!    [`select_by_name`] matcher, and the length within
//!    [`DURATION_TOLERANCE_MS`] when both sides know it. A title that only
//!    agrees once a version suffix is stripped ("… (Remastered 2011)",
//!    "… - Radio Edit") must also agree on the length, which is what tells
//!    a remaster from a live take. An entry without an artist is never
//!    matched by its title alone: "Intro" names a hundred tracks.
//!
//! Pure: the caller loads the [`Candidate`]s and writes the result.

use std::collections::HashMap;

use crate::metadata::name_match::{normalize_name, select_by_name};

/// How far two lengths may disagree and still be one recording: encoders
/// and catalogues round differently, and a gap of silence at the end is
/// trimmed by some and not others.
pub const DURATION_TOLERANCE_MS: i64 = 3_000;

/// One available library track, as the matcher sees it.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub track_id: i64,
    pub title: String,
    /// Every credited artist, one name per element (`track_artist`).
    pub artists: Vec<String>,
    pub duration_ms: i64,
    /// Through [`super::normalize_isrc`].
    pub isrc: Option<String>,
    /// The comparable form of the file's path, when the caller has
    /// entries that name paths (computing it touches the filesystem).
    pub path_key: Option<String>,
    /// The file name, lower-cased.
    pub file_name: Option<String>,
}

/// What is looked for.
#[derive(Debug, Clone, Default)]
pub struct Wanted<'a> {
    pub title: &'a str,
    pub artist: Option<&'a str>,
    pub duration_ms: Option<i64>,
    pub isrc: Option<&'a str>,
    /// The comparable form of the path an M3U named.
    pub path_key: Option<&'a str>,
    /// That path's file name, lower-cased.
    pub file_name: Option<&'a str>,
}

/// The library, indexed once for many lookups.
pub struct LibraryIndex {
    candidates: Vec<Candidate>,
    by_path: HashMap<String, usize>,
    by_file_name: HashMap<String, usize>,
    by_isrc: HashMap<String, Vec<usize>>,
    by_title: HashMap<String, Vec<usize>>,
    by_base_title: HashMap<String, Vec<usize>>,
}

/// A title with its version suffix taken off: whatever follows " - ", then
/// any trailing `( … )` / `[ … ]` groups. "Song (Live) [2011 Remaster]" and
/// "Song - Radio Edit" both become "Song".
fn base_title(title: &str) -> &str {
    let mut base = title.split(" - ").next().unwrap_or(title).trim_end();
    loop {
        let trimmed = base.trim_end();
        let open = match trimmed.chars().last() {
            Some(')') => '(',
            Some(']') => '[',
            _ => break,
        };
        match trimmed.rfind(open) {
            Some(at) if at > 0 => base = trimmed[..at].trim_end(),
            _ => break,
        }
    }
    if base.is_empty() {
        title
    } else {
        base
    }
}

/// The names an artist string credits. Exports write several artists in
/// one field with whatever separator they like; the whole string is kept
/// too, so a duo named "Simon & Garfunkel" still matches as one.
fn artist_names(raw: &str) -> Vec<String> {
    let mut names = vec![normalize_name(raw)];
    let lowered = raw.to_lowercase();
    let mut parts: Vec<String> = vec![lowered];
    for sep in [
        ";",
        ", ",
        " & ",
        " feat. ",
        " feat ",
        " ft. ",
        " featuring ",
        " x ",
        " / ",
        " with ",
    ] {
        parts = parts
            .iter()
            .flat_map(|p| p.split(sep).map(str::to_string).collect::<Vec<_>>())
            .collect();
    }
    names.extend(parts.iter().map(|p| normalize_name(p)));
    names.retain(|n| !n.is_empty());
    names.sort();
    names.dedup();
    names
}

impl LibraryIndex {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        let mut index = Self {
            by_path: HashMap::new(),
            by_file_name: HashMap::new(),
            by_isrc: HashMap::new(),
            by_title: HashMap::new(),
            by_base_title: HashMap::new(),
            candidates: Vec::new(),
        };
        for (at, candidate) in candidates.iter().enumerate() {
            if let Some(key) = &candidate.path_key {
                index.by_path.insert(key.clone(), at);
            }
            // Last write wins on a shared file name, as the M3U importer
            // always did; the user can still curate after the import.
            if let Some(name) = &candidate.file_name {
                index.by_file_name.insert(name.clone(), at);
            }
            if let Some(isrc) = &candidate.isrc {
                index.by_isrc.entry(isrc.clone()).or_default().push(at);
            }
            let title = normalize_name(&candidate.title);
            if !title.is_empty() {
                index.by_title.entry(title).or_default().push(at);
            }
            let base = normalize_name(base_title(&candidate.title));
            if !base.is_empty() {
                index.by_base_title.entry(base).or_default().push(at);
            }
        }
        index.candidates = candidates;
        index
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// How far a candidate's length is from the wanted one: `0` when either
    /// side does not know, so an unknown length never decides a tie.
    fn distance(&self, at: usize, wanted: Option<i64>) -> i64 {
        let have = self.candidates[at].duration_ms;
        match wanted {
            Some(want) if have > 0 => (have - want).abs(),
            _ => 0,
        }
    }

    /// The closest of `hits` by length, then the oldest row.
    fn closest(&self, hits: impl Iterator<Item = usize>, wanted: Option<i64>) -> Option<i64> {
        hits.min_by_key(|at| (self.distance(*at, wanted), self.candidates[*at].track_id))
            .map(|at| self.candidates[at].track_id)
    }

    fn artist_agrees(&self, at: usize, wanted: &[String]) -> bool {
        let credited = &self.candidates[at].artists;
        wanted
            .iter()
            .any(|name| select_by_name(credited.iter(), name, |a| Some(a.as_str())).is_some())
    }

    /// The track `wanted` is, if the library holds it.
    pub fn find(&self, wanted: &Wanted<'_>) -> Option<i64> {
        if let Some(at) = wanted.path_key.and_then(|key| self.by_path.get(key)) {
            return Some(self.candidates[*at].track_id);
        }
        if let Some(at) = wanted
            .file_name
            .and_then(|name| self.by_file_name.get(name))
        {
            return Some(self.candidates[*at].track_id);
        }
        if let Some(hits) = wanted.isrc.and_then(|isrc| self.by_isrc.get(isrc)) {
            if let Some(id) = self.closest(hits.iter().copied(), wanted.duration_ms) {
                return Some(id);
            }
        }

        let artists = artist_names(wanted.artist?);
        if artists.is_empty() {
            return None;
        }
        let within = |at: &usize, required: bool| {
            let have = self.candidates[*at].duration_ms;
            match wanted.duration_ms {
                Some(want) if have > 0 => (have - want).abs() <= DURATION_TOLERANCE_MS,
                _ => !required,
            }
        };
        let title = normalize_name(wanted.title);
        if let Some(hits) = self.by_title.get(&title) {
            let found = self.closest(
                hits.iter()
                    .copied()
                    .filter(|at| within(at, false) && self.artist_agrees(*at, &artists)),
                wanted.duration_ms,
            );
            if found.is_some() {
                return found;
            }
        }
        let base = normalize_name(base_title(wanted.title));
        self.by_base_title.get(&base).and_then(|hits| {
            self.closest(
                hits.iter()
                    .copied()
                    .filter(|at| within(at, true) && self.artist_agrees(*at, &artists)),
                wanted.duration_ms,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64, title: &str, artists: &[&str], duration_ms: i64) -> Candidate {
        Candidate {
            track_id: id,
            title: title.into(),
            artists: artists.iter().map(|a| a.to_string()).collect(),
            duration_ms,
            ..Candidate::default()
        }
    }

    fn wanted<'a>(title: &'a str, artist: Option<&'a str>, duration_ms: Option<i64>) -> Wanted<'a> {
        Wanted {
            title,
            artist,
            duration_ms,
            ..Wanted::default()
        }
    }

    #[test]
    fn the_isrc_wins_over_a_title_that_disagrees() {
        let mut tagged = track(2, "Totally different title", &["X"], 200_000);
        tagged.isrc = Some("GBAYE0601690".into());
        let index = LibraryIndex::new(vec![
            track(1, "Hey Jude", &["The Beatles"], 431_000),
            tagged,
        ]);
        let mut w = wanted("Hey Jude", Some("The Beatles"), Some(431_000));
        w.isrc = Some("GBAYE0601690");
        assert_eq!(index.find(&w), Some(2));
    }

    #[test]
    fn among_tracks_sharing_an_isrc_the_closest_length_wins() {
        let mut single = track(1, "A", &["B"], 180_000);
        single.isrc = Some("USRC17607839".into());
        let mut album = track(2, "A", &["B"], 240_000);
        album.isrc = Some("USRC17607839".into());
        let index = LibraryIndex::new(vec![single, album]);
        let mut w = wanted("A", None, Some(239_000));
        w.isrc = Some("USRC17607839");
        assert_eq!(index.find(&w), Some(2));
    }

    #[test]
    fn title_and_artist_match_across_case_accents_and_credits() {
        let index = LibraryIndex::new(vec![
            track(1, "Jóga", &["Björk"], 305_000),
            track(2, "Get Lucky", &["Daft Punk", "Pharrell Williams"], 369_000),
        ]);
        assert_eq!(
            index.find(&wanted("joga", Some("Bjork"), Some(304_000))),
            Some(1)
        );
        assert_eq!(
            index.find(&wanted(
                "Get Lucky",
                Some("Daft Punk feat. Pharrell Williams"),
                None
            )),
            Some(2)
        );
        // The wrong artist is not the same song.
        assert_eq!(
            index.find(&wanted("Get Lucky", Some("Someone Else"), None)),
            None
        );
    }

    #[test]
    fn a_length_too_far_off_is_another_recording() {
        let index = LibraryIndex::new(vec![track(1, "Song", &["Band"], 200_000)]);
        assert_eq!(
            index.find(&wanted("Song", Some("Band"), Some(260_000))),
            None
        );
        assert_eq!(
            index.find(&wanted("Song", Some("Band"), Some(202_000))),
            Some(1)
        );
    }

    #[test]
    fn a_version_suffix_matches_only_on_the_same_length() {
        let index = LibraryIndex::new(vec![track(1, "Song", &["Band"], 200_000)]);
        assert_eq!(
            index.find(&wanted(
                "Song (Remastered 2011)",
                Some("Band"),
                Some(201_000)
            )),
            Some(1)
        );
        assert_eq!(
            index.find(&wanted("Song - Live", Some("Band"), Some(320_000))),
            None
        );
        // Without a length to check, a suffix is not enough.
        assert_eq!(
            index.find(&wanted("Song - Radio Edit", Some("Band"), None)),
            None
        );
    }

    #[test]
    fn an_entry_without_artist_or_isrc_is_never_matched_by_title() {
        let index = LibraryIndex::new(vec![track(1, "Intro", &["Anyone"], 60_000)]);
        assert_eq!(index.find(&wanted("Intro", None, Some(60_000))), None);
    }

    #[test]
    fn a_path_or_file_name_wins_first() {
        let mut by_path = track(1, "Unrelated", &["Z"], 1);
        by_path.path_key = Some("c:\\music\\a.flac".into());
        by_path.file_name = Some("a.flac".into());
        let index = LibraryIndex::new(vec![by_path, track(2, "Song", &["Band"], 200_000)]);
        let mut w = wanted("Song", Some("Band"), None);
        w.path_key = Some("c:\\music\\a.flac");
        assert_eq!(index.find(&w), Some(1));
        let mut w = wanted("Song", Some("Band"), None);
        w.path_key = Some("d:\\moved\\a.flac");
        w.file_name = Some("a.flac");
        assert_eq!(index.find(&w), Some(1));
    }

    #[test]
    fn base_titles() {
        assert_eq!(base_title("Song (Live) [2011 Remaster]"), "Song");
        assert_eq!(base_title("Song - Radio Edit"), "Song");
        assert_eq!(base_title("(Untitled)"), "(Untitled)");
        assert_eq!(base_title("Plain"), "Plain");
    }
}
