//! The library enrichment engine: MusicBrainz identifiers for the local
//! library, and the gaps they fill — genres, release dates, credits,
//! covers, artist facts.
//!
//! Four parts, so each can be reused without the others:
//!
//! - [`matching`] — is this release the local album? Pure arithmetic over
//!   [`crate::metadata::album_match`] and the shared name matcher. No
//!   network, no database: the tag workshop reuses it as is.
//! - [`extract`] — what a MusicBrainz release contributes once matched
//!   (`musicbrainz` feature).
//! - [`resolve`] — searching the catalogue for an album or an artist and
//!   reaching a verdict (`musicbrainz` feature).
//! - [`store`] — the decisions, and filling the gaps they allow, in the
//!   profile database (`sqlite` feature).
//!
//! The engine writes the database only, never an audio file. How it
//! runs, what it applies on its own and why: `docs/features/enrichment.md`.

pub mod matching;

#[cfg(feature = "musicbrainz")]
pub mod extract;
#[cfg(feature = "musicbrainz")]
pub mod resolve;
#[cfg(feature = "sqlite")]
pub mod store;
#[cfg(feature = "musicbrainz")]
pub mod tags;

use crate::metadata::album_match::TrackSignals;

/// A local album, as the engine compares it.
#[derive(Debug, Clone)]
pub struct LocalAlbum {
    pub album_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub tracks: Vec<LocalTrack>,
}

#[derive(Debug, Clone)]
pub struct LocalTrack {
    pub track_id: i64,
    pub signals: TrackSignals,
    pub file_path: String,
}

impl LocalAlbum {
    /// The album as [`matching`] reads it.
    ///
    /// The individual names are the display credit split on `"; "` — the
    /// library's one multi-artist separator — and on `", "` too. Not as a
    /// storage rule, which stays `"; "` alone: an album artist is kept as
    /// the tag wrote it, and "Zedd, Alessia Cara" is a common way to write
    /// one. Here a fragment only ever has to equal a whole credited name
    /// under the shared matcher, so splitting "Earth, Wind & Fire" costs
    /// nothing: "Earth" names nobody on that release.
    pub fn side(&self) -> matching::AlbumSide {
        matching::AlbumSide {
            title: self.title.clone(),
            artist: self.artist.clone(),
            artist_names: self
                .artist
                .as_deref()
                .map(|a| {
                    a.split("; ")
                        .flat_map(|part| part.split(", "))
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            tracks: self.tracks.iter().map(|t| t.signals.clone()).collect(),
        }
    }
}

/// One credit of a track: who did what on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credit {
    /// MusicBrainz's relationship type: `composer`, `producer`, `vocal`…
    pub role: String,
    pub name: String,
    /// The instrument or vocal part, comma-joined; empty when the role
    /// has none.
    pub detail: String,
    pub artist_mbid: Option<String>,
}

/// A local track paired with a recording.
#[derive(Debug, Clone)]
pub struct MatchedTrack {
    pub track_id: i64,
    pub recording_mbid: String,
    pub score: f64,
    pub credits: Vec<Credit>,
}

/// A MusicBrainz artist proposed for a library artist.
#[derive(Debug, Clone, Default)]
pub struct ArtistProposal {
    pub mbid: String,
    pub name: String,
    pub disambiguation: Option<String>,
    pub kind: Option<String>,
    pub country: Option<String>,
    pub score: f64,
    /// Matched albums crediting this artist under this name; zero for a
    /// proposal that came from a name search alone.
    pub album_evidence: i64,
}

/// What an accepted artist shows on its page.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[cfg_attr(feature = "sqlite", derive(sqlx::FromRow))]
pub struct ArtistFacts {
    pub artist_mbid: Option<String>,
    pub name: Option<String>,
    pub disambiguation: Option<String>,
    pub kind: Option<String>,
    pub country: Option<String>,
    pub area: Option<String>,
    pub begin_date: Option<String>,
    pub end_date: Option<String>,
    /// A JSON array of genre names.
    pub genres: Option<String>,
}

/// What the engine fills from a matched release.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumFacts {
    pub release_mbid: String,
    pub release_group_mbid: Option<String>,
    pub release_title: String,
    pub release_artist: String,
    pub release_country: Option<String>,
    pub release_track_count: i64,
    /// The release group's first release date, `YYYY`, `YYYY-MM` or
    /// `YYYY-MM-DD` as the catalogue has it; this release's own date when
    /// the group has none.
    pub original_date: Option<String>,
    pub original_year: Option<i64>,
    pub genres: Vec<String>,
    pub has_front_cover: bool,
}
