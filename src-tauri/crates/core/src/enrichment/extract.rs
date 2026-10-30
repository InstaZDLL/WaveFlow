//! What a MusicBrainz release contributes: the side [`super::matching`]
//! compares, and the facts the engine fills once it is matched.

use crate::metadata::album_match::TrackSignals;
use crate::metadata::musicbrainz::{credit_text, ArtistRef, Genre, Relation, Release, Track};

use super::matching::AlbumSide;
use super::{AlbumFacts, Credit};

/// At most this many genres reach an album.
pub const MAX_GENRES: usize = 3;
/// At most this many credits are kept for one track. A recording with a
/// full orchestra listed would otherwise fill the properties panel.
pub const MAX_CREDITS: usize = 40;

/// One track of a release, where it sits on its disc.
#[derive(Debug, Clone)]
pub struct ReleaseTrack<'a> {
    /// The track as the matcher reads it.
    pub signals: TrackSignals,
    pub track: &'a Track,
    /// The disc's number: its position, or its rank when it has none.
    pub disc: i64,
    /// How many tracks that disc holds.
    pub disc_tracks: usize,
}

/// The release's tracks in disc order — the one walk every index into
/// "the release's tracks" refers to: the matcher's pairings
/// ([`remote_tracks`]) and the tag workshop's tags read the same list.
pub fn release_tracks(release: &Release) -> Vec<ReleaseTrack<'_>> {
    let multi_disc = release.media.len() > 1;
    let mut out = Vec::with_capacity(release.track_count());
    for (index, medium) in release.media.iter().enumerate() {
        let disc = medium.position.unwrap_or(index as i64 + 1);
        for track in &medium.tracks {
            let length = track
                .length
                .or_else(|| track.recording.as_ref().and_then(|r| r.length));
            out.push(ReleaseTrack {
                signals: TrackSignals {
                    title: track.title.clone(),
                    duration_ms: length,
                    track_number: track.position,
                    // Only when the release has several discs: a lone
                    // disc's "1" says nothing a local file without a
                    // disc number disagrees with.
                    disc_number: multi_disc.then_some(disc),
                },
                track,
                disc,
                disc_tracks: medium.tracks.len(),
            });
        }
    }
    out
}

/// The release's tracks in disc order, as the matcher reads them, with
/// the catalogue track each one came from.
pub fn remote_tracks(release: &Release) -> Vec<(TrackSignals, &Track)> {
    release_tracks(release)
        .into_iter()
        .map(|t| (t.signals, t.track))
        .collect()
}

/// The release as the album matcher sees it.
pub fn remote_side(release: &Release) -> AlbumSide {
    AlbumSide {
        title: release.title.clone(),
        artist: Some(credit_text(&release.artist_credit)).filter(|s| !s.is_empty()),
        artist_names: release
            .artist_credit
            .iter()
            .map(|c| {
                if c.name.is_empty() {
                    c.artist.name.clone()
                } else {
                    c.name.clone()
                }
            })
            .collect(),
        tracks: remote_tracks(release).into_iter().map(|(s, _)| s).collect(),
    }
}

pub fn album_facts(release: &Release) -> AlbumFacts {
    let group = release.release_group.as_ref();
    let original_date = group
        .and_then(|g| g.first_release_date.clone())
        .filter(|d| !d.is_empty())
        .or_else(|| release.date.clone().filter(|d| !d.is_empty()));
    let original_year = original_date.as_deref().and_then(year_of);
    let group_genres = group.map(|g| g.genres.as_slice()).unwrap_or_default();
    let genres = if group_genres.is_empty() {
        top_genres(&release.genres)
    } else {
        top_genres(group_genres)
    };
    AlbumFacts {
        release_mbid: release.id.clone(),
        release_group_mbid: group.map(|g| g.id.clone()),
        release_title: release.title.clone(),
        release_artist: credit_text(&release.artist_credit),
        release_country: release.country.clone(),
        release_track_count: release.track_count() as i64,
        original_date,
        original_year,
        genres,
        has_front_cover: release.cover_art_archive.as_ref().is_some_and(|c| c.front),
    }
}

/// The year of a `YYYY[-MM[-DD]]` date. A year before recorded sound or
/// past the next one is a typo, not a date.
pub fn year_of(date: &str) -> Option<i64> {
    let year: i64 = date.get(0..4)?.parse().ok()?;
    (1860..=2100).contains(&year).then_some(year)
}

/// The genres worth filling: the most voted first, cut to
/// [`MAX_GENRES`], and only those with at least a quarter of the top
/// genre's votes — one editor's stray tag beside fifty votes for
/// another is noise, not a second genre.
pub fn top_genres(genres: &[Genre]) -> Vec<String> {
    let mut sorted: Vec<&Genre> = genres
        .iter()
        .filter(|g| !g.name.trim().is_empty())
        .collect();
    sorted.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    let Some(top) = sorted.first().map(|g| g.count.max(1)) else {
        return Vec::new();
    };
    sorted
        .into_iter()
        .filter(|g| g.count.max(1) * 4 >= top)
        .take(MAX_GENRES)
        .map(|g| display_genre(&g.name))
        .collect()
}

/// MusicBrainz names genres in lower case ("alternative rock"); a
/// library shows them capitalised. Each word, and each part of a
/// hyphenated word, gets an upper-case first letter: "Lo-Fi", "Hip Hop".
/// An all-capital word the catalogue already wrote ("EDM") is kept.
pub fn display_genre(name: &str) -> String {
    name.trim()
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|word| {
            word.split('-')
                .map(capitalise)
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalise(part: &str) -> String {
    let mut chars = part.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Relationship types kept as credits, at recording level.
const RECORDING_ROLES: &[&str] = &[
    "producer",
    "mix",
    "engineer",
    "vocal",
    "instrument",
    "performer",
    "arranger",
    "orchestrator",
    "conductor",
    "remixer",
];
/// …and at work level: who wrote the song.
const WORK_ROLES: &[&str] = &["composer", "lyricist", "writer", "arranger"];

/// The credits of one catalogue track: the writers of its work first,
/// then the performers and the people behind the desk, in the order the
/// catalogue lists them, without repeats.
pub fn track_credits(track: &Track) -> Vec<Credit> {
    let mut out: Vec<Credit> = Vec::new();
    let Some(recording) = &track.recording else {
        return out;
    };
    for relation in &recording.relations {
        if let Some(work) = relation
            .work
            .as_ref()
            .filter(|_| relation.target_type == "work")
        {
            for inner in &work.relations {
                push_credit(&mut out, inner, WORK_ROLES);
            }
        }
    }
    for relation in &recording.relations {
        push_credit(&mut out, relation, RECORDING_ROLES);
    }
    out.truncate(MAX_CREDITS);
    out
}

fn push_credit(out: &mut Vec<Credit>, relation: &Relation, roles: &[&str]) {
    if relation.target_type != "artist" || !roles.contains(&relation.kind.as_str()) {
        return;
    }
    let Some(artist) = &relation.artist else {
        return;
    };
    let name = relation
        .target_credit
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(&artist.name)
        .trim()
        .to_owned();
    if name.is_empty() {
        return;
    }
    let credit = Credit {
        role: relation.kind.clone(),
        name,
        detail: relation.attributes.join(", "),
        artist_mbid: Some(artist.id.clone()).filter(|id| !id.is_empty()),
    };
    if !out
        .iter()
        .any(|c| c.role == credit.role && c.name == credit.name && c.detail == credit.detail)
    {
        out.push(credit);
    }
}

/// The artists the release credits, for the artist proposals.
pub fn credited_artists(release: &Release) -> Vec<(String, ArtistRef)> {
    release
        .artist_credit
        .iter()
        .filter(|c| !c.artist.id.is_empty())
        .map(|c| {
            let credited = if c.name.is_empty() {
                c.artist.name.clone()
            } else {
                c.name.clone()
            };
            (credited, c.artist.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests;
