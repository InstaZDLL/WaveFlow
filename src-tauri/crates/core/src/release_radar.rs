//! Release Radar: the recent releases of the artists in the library,
//! found on MusicBrainz and matched against the files the user already
//! has.
//!
//! A release is in one of two states, and only two:
//!
//! - **discovered** — MusicBrainz knows of it, the library holds none of
//!   it. Shown with a link to its MusicBrainz page.
//! - **playable** — at least one local track was matched to it. Only
//!   these feed the "Release Radar" smart playlist
//!   ([`crate::smart_playlists::release_radar`]).
//!
//! Nothing is downloaded but the release's information and its cover:
//! the radar points at music, it never fetches any.
//!
//! The pieces here are pure (the query, the parsing, the title rules,
//! the matching), the client is in [`musicbrainz`] and the tables in
//! [`store`]. The scan itself — pacing, the offline switch, the task
//! row, the profile it runs for — is the app's
//! (`commands/release_radar.rs`), because it needs all four.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::metadata::name_match::{normalize_name, select_by_name};

pub mod musicbrainz;
#[cfg(feature = "sqlite")]
pub mod store;

/// A release is recent when it first came out within this many days.
pub const RECENT_DAYS: i64 = 180;

/// How many of the library's artists the radar follows, by weight
/// ([`store::followed_artists`]). Bounded so a large library is a minute
/// or two of MusicBrainz queries, not an hour.
pub const FOLLOWED_ARTISTS: i64 = 100;

/// An artist asked about less than this long ago is not asked again by
/// an ordinary scan: MusicBrainz does not change by the minute, and a
/// scan stopped halfway resumes with the artists it did not reach.
pub const REFRESH_AFTER_MS: i64 = 20 * 60 * 60 * 1000;

/// A cover the Cover Art Archive did not have is asked for again after
/// this long — art is often uploaded a few days after the release.
pub const COVER_RETRY_AFTER_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The Cover Art Archive front of a release group, at the size the view
/// draws it.
pub fn cover_url(release_group_id: &str) -> String {
    format!("https://coverartarchive.org/release-group/{release_group_id}/front-250")
}

/// The release group's page on MusicBrainz.
pub fn musicbrainz_url(release_group_id: &str) -> String {
    format!("https://musicbrainz.org/release-group/{release_group_id}")
}

/// `YYYY-MM-DD`, `days` before `now`.
pub fn cutoff_date(now: DateTime<Utc>, days: i64) -> String {
    (now - Duration::days(days)).format("%Y-%m-%d").to_string()
}

/// A partial MusicBrainz date as a comparable `YYYY-MM-DD` lower bound,
/// so a string comparison against [`cutoff_date`] is sound: `"2026"`
/// reads as `"2026-01-01"`, `"2026-05"` as `"2026-05-01"`.
pub fn date_key(date: &str) -> String {
    match date.len() {
        4 => format!("{date}-01-01"),
        7 => format!("{date}-01"),
        _ => date.to_string(),
    }
}

/// Who a query is about: a MusicBrainz id once one is known, the name
/// until then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtistQuery<'a> {
    Mbid(&'a str),
    Name(&'a str),
}

/// The Lucene query for an artist's release groups out since `cutoff`.
///
/// By id when there is one (`arid:`), which no homonym can share; by
/// name otherwise, as a quoted phrase, the credit then checked by
/// [`parse_release_groups`].
pub fn lucene_query(artist: &ArtistQuery<'_>, cutoff: &str) -> String {
    let who = match artist {
        ArtistQuery::Mbid(mbid) => format!("arid:{}", escape_phrase(mbid)),
        ArtistQuery::Name(name) => format!("artist:\"{}\"", escape_phrase(name)),
    };
    format!("{who} AND firstreleasedate:[{cutoff} TO *]")
}

/// Escape a value placed inside a Lucene quoted phrase, or used as a
/// bare term.
fn escape_phrase(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One release group, as the radar keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseGroup {
    /// The release group's MusicBrainz id.
    pub id: String,
    pub title: String,
    /// The whole credit, names and join phrases ("A feat. B").
    pub artist_credit: String,
    pub first_release_date: String,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    /// The MusicBrainz id of the credit that carries the artist's exact
    /// name, for a query by name: what [`confirmed_mbid`] may learn.
    /// `None` when the artist is only credited under a longer name
    /// ("Artist & The Band" is the band's id, not the artist's).
    pub credit_mbid: Option<String>,
}

/// What one answer holds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParsedPage {
    pub releases: Vec<ReleaseGroup>,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(rename = "release-groups", default)]
    release_groups: Vec<RawReleaseGroup>,
}

#[derive(Deserialize)]
struct RawReleaseGroup {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "first-release-date", default)]
    first_release_date: String,
    #[serde(rename = "primary-type")]
    primary_type: Option<String>,
    #[serde(rename = "secondary-types", default)]
    secondary_types: Vec<String>,
    #[serde(rename = "artist-credit", default)]
    artist_credit: Vec<RawCredit>,
}

#[derive(Deserialize)]
struct RawCredit {
    #[serde(default)]
    name: String,
    #[serde(default)]
    joinphrase: String,
    artist: Option<RawArtist>,
}

#[derive(Deserialize)]
struct RawArtist {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

/// The release groups of one answer that belong to the artist and came
/// out on or after `cutoff`.
///
/// A query by name is a phrase search, and MusicBrainz answers it with
/// homonyms and with releases that only mention the name, so a release
/// is kept only when one of its credits is the artist: by id when the
/// query was by id, by [`select_by_name`] otherwise — the matcher every
/// metadata provider uses, which also accepts "Artist & The Band".
/// A release with no date is dropped, since it cannot be called recent.
pub fn parse_release_groups(
    body: &[u8],
    artist: &ArtistQuery<'_>,
    cutoff: &str,
) -> Result<ParsedPage, serde_json::Error> {
    let response: SearchResponse = serde_json::from_slice(body)?;
    let searched = match artist {
        ArtistQuery::Name(name) => normalize_name(name),
        ArtistQuery::Mbid(_) => String::new(),
    };
    let mut releases = Vec::new();
    for group in response.release_groups {
        if group.first_release_date.is_empty()
            || date_key(&group.first_release_date).as_str() < cutoff
        {
            continue;
        }
        let (credited, credit_mbid) = match artist {
            ArtistQuery::Mbid(mbid) => (
                group
                    .artist_credit
                    .iter()
                    .any(|c| c.artist.as_ref().is_some_and(|a| a.id == *mbid)),
                None,
            ),
            ArtistQuery::Name(_) => {
                let matched = select_by_name(group.artist_credit.iter(), &searched, |c| {
                    Some(credit_name(c))
                });
                let exact = matched
                    .filter(|c| normalize_name(credit_name(c)) == searched)
                    .and_then(|c| c.artist.as_ref())
                    .map(|a| a.id.clone())
                    .filter(|id| !id.is_empty());
                (matched.is_some(), exact)
            }
        };
        if !credited {
            continue;
        }
        let artist_credit = full_credit(&group.artist_credit);
        releases.push(ReleaseGroup {
            id: group.id,
            title: group.title,
            artist_credit,
            first_release_date: group.first_release_date,
            primary_type: group.primary_type.filter(|t| !t.is_empty()),
            secondary_types: group.secondary_types,
            credit_mbid,
        });
    }
    Ok(ParsedPage { releases })
}

/// The artist's MusicBrainz id, when the library confirms it: every
/// release of the answer that matched a local track of the artist was
/// credited, under the artist's exact name, to one and the same id.
///
/// **The library is the proof, not the answer.** The answer is filtered
/// to the recent window, so an artist with nothing new and a homonym
/// with something would leave the homonym's id alone on the page, and
/// learning it would send every later query to the homonym. A homonym
/// does not put out a record the user already has under the artist's
/// name; so an id is learnt only off a release the library matched, and
/// until then the radar keeps searching by name.
pub fn confirmed_mbid(
    releases: &[ReleaseGroup],
    matches: &HashMap<String, LocalMatch>,
) -> Option<String> {
    let ids: HashSet<&str> = releases
        .iter()
        .filter(|r| matches.contains_key(&r.id))
        .filter_map(|r| r.credit_mbid.as_deref())
        .collect();
    if ids.len() == 1 {
        ids.into_iter().next().map(str::to_string)
    } else {
        None
    }
}

/// The name a credit shows, falling back to the artist's own name when
/// the credit leaves it out.
fn credit_name(credit: &RawCredit) -> &str {
    if credit.name.is_empty() {
        credit.artist.as_ref().map_or("", |a| a.name.as_str())
    } else {
        credit.name.as_str()
    }
}

fn full_credit(credits: &[RawCredit]) -> String {
    let mut out = String::new();
    for credit in credits {
        out.push_str(credit_name(credit));
        out.push_str(&credit.joinphrase);
    }
    out.trim().to_string()
}

/// The words that mark a bracketed suffix as an edition of the same
/// record rather than a different one: "(Deluxe Edition)", "[2024
/// Remaster]". A remix or a live recording is another record, and keeps
/// its suffix.
const EDITION_WORDS: [&str; 9] = [
    "deluxe",
    "edition",
    "remaster",
    "remastered",
    "expanded",
    "bonus",
    "anniversary",
    "explicit",
    "clean",
];

/// A release or album title reduced to what identifies the record:
/// a store's " - Single" / " - EP" suffix and a trailing bracketed
/// edition dropped, then [`normalize_name`]. "Album (Deluxe Edition)",
/// "Album" and "album" are one key; "Song (Remix)" and "Song" are two.
pub fn title_key(title: &str) -> String {
    let mut t = title.trim();
    loop {
        let before = t;
        for suffix in [" - single", " - ep"] {
            // Compared on bytes, not on a lowercased copy: lowercasing
            // can change a string's length, and the cut is made in `t`.
            let Some(cut) = t.len().checked_sub(suffix.len()) else {
                continue;
            };
            if t.is_char_boundary(cut) && t[cut..].eq_ignore_ascii_case(suffix) {
                t = t[..cut].trim_end();
                break;
            }
        }
        if let Some(stripped) = strip_edition_bracket(t) {
            t = stripped;
        }
        if t == before {
            break;
        }
    }
    normalize_name(t)
}

fn strip_edition_bracket(t: &str) -> Option<&str> {
    let (open, close) = match t.chars().last()? {
        ')' => ('(', ')'),
        ']' => ('[', ']'),
        _ => return None,
    };
    let start = t.rfind(open)?;
    let inner = t[start + open.len_utf8()..t.len() - close.len_utf8()].to_lowercase();
    let words = normalize_name(&inner);
    let is_edition = words.split(' ').any(|w| EDITION_WORDS.contains(&w));
    let rest = t[..start].trim_end();
    (is_edition && !rest.is_empty()).then_some(rest)
}

/// A release as the matcher needs it.
#[derive(Debug, Clone)]
pub struct ReleaseRef {
    pub id: String,
    /// The library artist it was found for.
    pub artist_id: i64,
    pub title: String,
    pub primary_type: Option<String>,
}

/// A local track by one of the followed artists.
#[derive(Debug, Clone)]
pub struct LocalTrack {
    pub track_id: i64,
    /// The artist the row is about — a track credits several, and an
    /// album has its own artist; one row per pairing.
    pub artist_id: i64,
    pub title: String,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
}

/// How a release found its local tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    /// A local album by the artist carries the release's title: the
    /// tracks play as the record they are.
    Album,
    /// A single, found by the title of a track of the artist's — often
    /// on the album that came after it.
    Track,
}

/// The local tracks of one release, in the order they play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMatch {
    pub track_ids: Vec<i64>,
    pub matched_by: MatchedBy,
}

/// Match every release against the library's tracks.
///
/// Within the artist the release was found for, never across artists:
/// a title alone ("Intro", "Home") is shared by half a library. A
/// release matches the local albums with its [`title_key`]; a single
/// that no album matches falls back to the artist's tracks of that
/// title. An album never falls back to a track: the title track of a
/// record is not the record.
///
/// A release with no match is absent from the map — that, and nothing
/// else, is what "discovered" means.
pub fn match_releases(
    releases: &[ReleaseRef],
    tracks: &[LocalTrack],
) -> HashMap<String, LocalMatch> {
    // (artist, key) → the tracks under it, once per track.
    let mut by_album: HashMap<(i64, String), Vec<&LocalTrack>> = HashMap::new();
    let mut by_title: HashMap<(i64, String), Vec<&LocalTrack>> = HashMap::new();
    for track in tracks {
        if let Some(album) = track.album_title.as_deref() {
            let key = title_key(album);
            if !key.is_empty() {
                by_album
                    .entry((track.artist_id, key))
                    .or_default()
                    .push(track);
            }
        }
        let key = title_key(&track.title);
        if !key.is_empty() {
            by_title
                .entry((track.artist_id, key))
                .or_default()
                .push(track);
        }
    }

    let mut out = HashMap::new();
    for release in releases {
        let key = title_key(&release.title);
        if key.is_empty() {
            continue;
        }
        let lookup = (release.artist_id, key);
        let (found, matched_by) = match by_album.get(&lookup) {
            Some(found) => (found, MatchedBy::Album),
            None if is_single(release.primary_type.as_deref()) => match by_title.get(&lookup) {
                Some(found) => (found, MatchedBy::Track),
                None => continue,
            },
            None => continue,
        };
        let mut ordered: Vec<&LocalTrack> = found.clone();
        ordered.sort_by_key(|t| {
            (
                t.album_id,
                t.disc_number.unwrap_or(1),
                t.track_number.unwrap_or(i64::MAX),
                t.track_id,
            )
        });
        let mut seen = HashSet::new();
        let track_ids: Vec<i64> = ordered
            .into_iter()
            .map(|t| t.track_id)
            .filter(|id| seen.insert(*id))
            .collect();
        out.insert(
            release.id.clone(),
            LocalMatch {
                track_ids,
                matched_by,
            },
        );
    }
    out
}

fn is_single(primary_type: Option<&str>) -> bool {
    primary_type.is_some_and(|t| t.eq_ignore_ascii_case("single"))
}

#[cfg(test)]
mod tests;
