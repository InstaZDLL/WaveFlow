//! Finding the release a local album is, and the artist a library name
//! is — the part of the engine that talks to the catalogue.
//!
//! No database here: the caller loads the album, hands it over, and
//! writes what comes back. That keeps the network wait out of any
//! transaction, and lets a server run the same resolution.

use crate::metadata::musicbrainz::{ArtistRef, ArtistSummary, MusicBrainzClient, Release};
use crate::metadata::name_match::{normalize_name, select_by_name};
use crate::metadata::LookupError;

use super::extract;
use super::matching::{self, AlbumScore, Verdict};
use super::AlbumFacts;
use super::{LocalAlbum, MatchedTrack};

/// Hits asked of a release search.
pub const SEARCH_LIMIT: u32 = 10;
/// Releases looked up for one album, at most. Each is a second of the
/// service's budget; past the third-best hit the right release is
/// rarely further down.
pub const MAX_LOOKUPS: usize = 3;
/// A hit whose summary scores below this is not worth its lookup.
pub const MIN_PRESCORE: f64 = 0.5;

/// What the catalogue says about one local album.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub verdict: Verdict,
    pub score: AlbumScore,
    pub facts: AlbumFacts,
    pub tracks: Vec<MatchedTrack>,
    /// The artists the release credits, as `(credited name, artist)`.
    pub credited: Vec<(String, ArtistRef)>,
    /// The release was named by the files' own tags.
    pub from_tags: bool,
}

/// Compare a local album with one release.
///
/// A release the files themselves name (`from_tags`) is trusted for its
/// identity: the verdict is confident when every local track pairs with
/// one of its recordings, whatever the title or the credit say — a
/// tagger wrote that identifier knowing which pressing it had. Without
/// the pairing it is only a proposal, because a file can be retagged by
/// hand and keep a stale identifier.
pub fn resolution(local: &LocalAlbum, release: &Release, from_tags: bool) -> Resolution {
    let remote_tracks = extract::remote_tracks(release);
    let remote = extract::remote_side(release);
    let found = matching::match_album(&local.side(), &remote);
    let verdict = verdict_for(&found, from_tags);
    let tracks = found
        .assignments
        .iter()
        .filter_map(|a| {
            let (_, track) = remote_tracks.get(a.remote)?;
            let recording = track.recording.as_ref()?;
            Some(MatchedTrack {
                track_id: local.tracks.get(a.local)?.track_id,
                recording_mbid: recording.id.clone(),
                score: a.score,
                credits: extract::track_credits(track),
            })
        })
        .collect();
    Resolution {
        verdict,
        score: found.score,
        facts: extract::album_facts(release),
        tracks,
        credited: extract::credited_artists(release),
        from_tags,
    }
}

/// The verdict a match earns: the matcher's own, except for a release the
/// files name, which is trusted for its identity — confident when every
/// local track pairs, a proposal otherwise (see [`resolution`]). Shared
/// with the tag workshop, so both read one release the same way.
pub fn verdict_for(found: &matching::AlbumMatch, from_tags: bool) -> Verdict {
    if !from_tags {
        return found.verdict;
    }
    if found.score.local_tracks > 0 && found.score.paired == found.score.local_tracks {
        Verdict::Confident
    } else {
        Verdict::Doubtful
    }
}

fn verdict_rank(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::Confident => 2,
        Verdict::Doubtful => 1,
        Verdict::NoMatch => 0,
    }
}

/// The better of two resolutions: the stronger verdict, then the higher
/// score.
fn better(a: &Resolution, b: &Resolution) -> bool {
    (verdict_rank(a.verdict), a.score.total) > (verdict_rank(b.verdict), b.score.total)
}

/// Resolve one album: the release its files name, or else the best of a
/// search.
///
/// `Ok(None)` when the catalogue has nothing worth proposing — a real
/// answer, remembered as "no match". An `Err` is a failure to ask, and is
/// not remembered at all.
pub async fn resolve_album(
    client: &MusicBrainzClient,
    local: &LocalAlbum,
    tagged_release: Option<&str>,
) -> Result<Option<Resolution>, LookupError> {
    Ok(resolve_album_release(client, local, tagged_release)
        .await?
        .map(|(resolution, _)| resolution))
}

/// [`resolve_album`], with the release the verdict is about: the tag
/// workshop reads its tracklist to propose each file's tags, and asking
/// the catalogue a second time would cost a second of its budget.
pub async fn resolve_album_release(
    client: &MusicBrainzClient,
    local: &LocalAlbum,
    tagged_release: Option<&str>,
) -> Result<Option<(Resolution, Release)>, LookupError> {
    if let Some(mbid) = tagged_release {
        if let Some(release) = client.get_release(mbid).await? {
            return Ok(Some((resolution(local, &release, true), release)));
        }
    }

    let side = local.side();
    let hits = client
        .search_releases(&local.title, local.artist.as_deref(), SEARCH_LIMIT)
        .await?;
    let mut ranked: Vec<(f64, &crate::metadata::musicbrainz::ReleaseSummary)> = hits
        .iter()
        .map(|hit| {
            let artist = crate::metadata::musicbrainz::credit_text(&hit.artist_credit);
            (
                matching::prescore(&side, &hit.title, &artist, hit.track_count),
                hit,
            )
        })
        .filter(|(score, _)| *score >= MIN_PRESCORE)
        .collect();
    // Stable: equal scores keep the service's relevance order.
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut best: Option<(Resolution, Release)> = None;
    for (_, hit) in ranked.into_iter().take(MAX_LOOKUPS) {
        let Some(release) = client.get_release(&hit.id).await? else {
            continue;
        };
        let candidate = resolution(local, &release, false);
        if candidate.verdict == Verdict::NoMatch {
            continue;
        }
        let confident = candidate.verdict == Verdict::Confident;
        if best.as_ref().map_or(true, |(b, _)| better(&candidate, b)) {
            best = Some((candidate, release));
        }
        if confident {
            break;
        }
    }
    Ok(best)
}

/// Resolve an album against one known release: accepting a proposal
/// looks it up again, so what is applied is what the catalogue says now.
pub async fn resolve_against(
    client: &MusicBrainzClient,
    local: &LocalAlbum,
    release_mbid: &str,
) -> Result<Option<Resolution>, LookupError> {
    Ok(resolve_against_release(client, local, release_mbid)
        .await?
        .map(|(resolution, _)| resolution))
}

/// [`resolve_against`], with the release: see [`resolve_album_release`].
pub async fn resolve_against_release(
    client: &MusicBrainzClient,
    local: &LocalAlbum,
    release_mbid: &str,
) -> Result<Option<(Resolution, Release)>, LookupError> {
    Ok(client
        .get_release(release_mbid)
        .await?
        .map(|release| (resolution(local, &release, false), release)))
}

/// A proposal for a library artist found by name alone.
///
/// The shared matcher picks the hit; the service's own relevance is the
/// score. When several hits carry the very same name — two bands called
/// Nirvana — the score is cut, so the review screen ranks the doubtful
/// ones last; the disambiguation it shows is what the user decides on.
pub async fn resolve_artist(
    client: &MusicBrainzClient,
    name: &str,
) -> Result<Option<super::ArtistProposal>, LookupError> {
    let searched = normalize_name(name);
    if searched.is_empty() {
        return Ok(None);
    }
    let hits = client.search_artists(name, 5).await?;
    let namesakes = hits
        .iter()
        .filter(|h| normalize_name(&h.name) == searched)
        .count();
    let Some(hit) = select_by_name(hits.iter(), &searched, |h| Some(h.name.as_str())) else {
        return Ok(None);
    };
    let mut score = hit.score.unwrap_or(0) as f64 / 100.0;
    if namesakes > 1 {
        score *= 0.6;
    }
    Ok(Some(super::ArtistProposal {
        mbid: hit.id.clone(),
        name: hit.name.clone(),
        disambiguation: hit.disambiguation.clone().filter(|d| !d.is_empty()),
        kind: hit.kind.clone(),
        country: hit.country.clone(),
        score: score.clamp(0.0, 1.0),
        album_evidence: 0,
    }))
}

/// The facts an accepted artist keeps, from its lookup.
pub fn artist_facts(artist: &ArtistSummary) -> super::ArtistFacts {
    let genres = extract::top_genres(&artist.genres);
    super::ArtistFacts {
        artist_mbid: Some(artist.id.clone()),
        name: Some(artist.name.clone()).filter(|n| !n.is_empty()),
        disambiguation: artist.disambiguation.clone().filter(|d| !d.is_empty()),
        kind: artist.kind.clone(),
        country: artist.country.clone(),
        area: artist
            .area
            .as_ref()
            .map(|a| a.name.clone())
            .filter(|a| !a.is_empty()),
        begin_date: artist.life_span.as_ref().and_then(|l| l.begin.clone()),
        end_date: artist.life_span.as_ref().and_then(|l| l.end.clone()),
        genres: (!genres.is_empty()).then(|| serde_json::to_string(&genres).unwrap_or_default()),
    }
}
