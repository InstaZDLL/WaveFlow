//! The tags a matched MusicBrainz release gives each file.
//!
//! The match itself is the enrichment engine's: the release comes from
//! [`crate::enrichment::resolve`], and the pairing of files with its
//! tracks from [`crate::enrichment::matching`], so the workshop and the
//! engine agree on which recording a file is. What is added here is
//! the step the engine never takes — turning a paired track into tags.

use crate::enrichment::matching::{self, AlbumScore, Verdict};
use crate::enrichment::LocalAlbum;
use crate::enrichment::{extract, resolve};
use crate::metadata::musicbrainz::{credit_text, ArtistCredit, Release};

use super::{Field, Values};

/// The release a proposal comes from, as the workshop shows it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReleaseInfo {
    pub mbid: String,
    pub release_group_mbid: Option<String>,
    pub title: String,
    pub artist: String,
    pub date: Option<String>,
    pub country: Option<String>,
    pub track_count: i64,
    pub has_front_cover: bool,
}

/// What one file would be tagged with.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackProposal {
    pub track_id: i64,
    /// How sure the pairing of this file with its track is, `0.0..=1.0`.
    pub score: f64,
    pub values: Values,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AlbumProposal {
    pub release: ReleaseInfo,
    pub verdict: Verdict,
    pub score: AlbumScore,
    /// The release was named by the files' own tags.
    pub from_tags: bool,
    pub tracks: Vec<TrackProposal>,
    /// Files no track of the release was paired with: nothing is
    /// proposed for them.
    pub unmatched: Vec<i64>,
}

/// The tags `release` proposes for each file of `local`.
///
/// Every field a tagger would set from the release: the track's title
/// and artists, the album, its artist, the year, the numbers and totals,
/// the genres the engine would fill, and the six identifiers. The year
/// is the release group's first — the album's year, as the engine fills
/// it — rather than this pressing's. A field the release says nothing
/// about is left out, which leaves the file's own value alone.
pub fn propose(local: &LocalAlbum, release: &Release, from_tags: bool) -> AlbumProposal {
    let found = matching::match_album(&local.side(), &extract::remote_side(release));
    let verdict = resolve::verdict_for(&found, from_tags);
    let facts = extract::album_facts(release);

    // The very walk the pairing's indices are into (`remote_side` reads
    // `remote_tracks`, which reads this one), with each track's disc.
    let remote = extract::release_tracks(release);

    let album_artist = credit_text(&release.artist_credit);
    let album_artist_ids = credit_ids(&release.artist_credit);
    let mut tracks = Vec::with_capacity(found.assignments.len());
    let mut paired = vec![false; local.tracks.len()];
    for assignment in &found.assignments {
        let (Some(file), Some(slot)) = (
            local.tracks.get(assignment.local),
            remote.get(assignment.remote),
        ) else {
            continue;
        };
        let (track, disc, on_disc) = (slot.track, slot.disc, slot.disc_tracks);
        paired[assignment.local] = true;
        let mut values = Values::new();
        let mut set = |field: Field, value: Option<String>| {
            if let Some(value) = value.filter(|v| !v.trim().is_empty()) {
                values.insert(field, value);
            }
        };
        // The track's own credit, else the release's: a lookup lists
        // one on every track, but an old entry may not.
        let credit = if track.artist_credit.is_empty() {
            &release.artist_credit
        } else {
            &track.artist_credit
        };
        set(Field::Title, Some(track.title.clone()));
        set(Field::Artist, Some(credit_names(credit)));
        set(Field::Album, Some(release.title.clone()));
        set(Field::AlbumArtist, Some(album_artist.clone()));
        set(Field::Year, facts.original_year.map(|y| y.to_string()));
        set(Field::TrackNumber, track.position.map(|n| n.to_string()));
        set(Field::TrackTotal, Some(on_disc.to_string()));
        set(Field::DiscNumber, Some(disc.to_string()));
        set(Field::DiscTotal, Some(release.media.len().to_string()));
        set(Field::Genre, Some(facts.genres.join("; ")));
        set(Field::MbReleaseId, Some(release.id.clone()));
        set(Field::MbReleaseGroupId, facts.release_group_mbid.clone());
        set(
            Field::MbRecordingId,
            track.recording.as_ref().map(|r| r.id.clone()),
        );
        set(Field::MbReleaseTrackId, Some(track.id.clone()));
        set(Field::MbArtistId, Some(credit_ids(credit)));
        set(Field::MbAlbumArtistId, Some(album_artist_ids.clone()));
        tracks.push(TrackProposal {
            track_id: file.track_id,
            score: assignment.score,
            values,
        });
    }
    let unmatched = local
        .tracks
        .iter()
        .zip(&paired)
        .filter(|(_, paired)| !**paired)
        .map(|(t, _)| t.track_id)
        .collect();

    AlbumProposal {
        release: ReleaseInfo {
            mbid: release.id.clone(),
            release_group_mbid: facts.release_group_mbid.clone(),
            title: release.title.clone(),
            artist: album_artist,
            date: release.date.clone().filter(|d| !d.is_empty()),
            country: release.country.clone(),
            track_count: release.track_count() as i64,
            has_front_cover: facts.has_front_cover,
        },
        verdict,
        score: found.score,
        from_tags,
        tracks,
        unmatched,
    }
}

/// The credited names, `"; "`-joined: the form the scanner splits back
/// into one artist each. The join phrases ("feat.", " & ") are the
/// credit's typography, not names, and are left out.
fn credit_names(credit: &[ArtistCredit]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for c in credit {
        let name = if c.name.trim().is_empty() {
            c.artist.name.trim()
        } else {
            c.name.trim()
        };
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names.join("; ")
}

fn credit_ids(credit: &[ArtistCredit]) -> String {
    let mut ids: Vec<&str> = Vec::new();
    for c in credit {
        let id = c.artist.id.trim();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.join("; ")
}
