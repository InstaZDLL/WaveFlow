//! Which library track an imported entry (or a placeholder) is.
//!
//! Strongest key first, each one tried only when the one before found
//! nothing:
//!
//! 1. **The path an M3U named** — its comparable form
//!    ([`super::path_key`]), then its file name, as the M3U importer always
//!    did: a playlist written before the library was reorganised still
//!    finds its files. The file name alone only when the entry names no
//!    artist and one file in the library has it, otherwise the file must be
//!    by that artist — `01.mp3` names a track in every album, and a
//!    placeholder is matched again after every scan.
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
    pub file_path: String,
    /// The comparable form of the file's path, when the caller has
    /// entries that name paths.
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
    by_file_name: HashMap<String, Vec<usize>>,
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
            if let Some(name) = &candidate.file_name {
                index.by_file_name.entry(name.clone()).or_default().push(at);
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
        if let Some(hits) = wanted
            .file_name
            .and_then(|name| self.by_file_name.get(name))
        {
            // A file name alone ("01.mp3") names a track in every album.
            // With nothing else to go on it is taken only when one file in
            // the library has it; when the entry names an artist, the file
            // must be by them, the closest length deciding between several.
            let found = match wanted.artist {
                None => (hits.len() == 1).then(|| self.candidates[hits[0]].track_id),
                Some(artist) => {
                    let names = artist_names(artist);
                    self.closest(
                        hits.iter()
                            .copied()
                            .filter(|at| self.artist_agrees(*at, &names)),
                        wanted.duration_ms,
                    )
                }
            };
            if found.is_some() {
                return found;
            }
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
mod tests;
