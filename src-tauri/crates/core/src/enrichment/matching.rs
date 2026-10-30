//! Is this catalogue release the local album? Arithmetic only.
//!
//! The track half is [`album_match`]: three weighted signals per track and
//! a global greedy pairing, so one generic title cannot swallow a record.
//! This module adds the album half — its title and its artist — and
//! turns the three into a verdict. Names are compared through the shared
//! matcher ([`name_match`]), so the engine and the tag workshop agree on
//! what "the same artist" means.
//!
//! # What "confident" means
//!
//! Confident is what the engine applies without asking. It needs all of:
//!
//! - the album titles agree ([`CONFIDENT_TITLE`]), after an edition
//!   suffix is set aside on both sides — "Nevermind (Remastered)" is
//!   still "Nevermind";
//! - the artist agrees ([`CONFIDENT_ARTIST`]): the shared matcher says
//!   yes for the album artist, or for one of the credited names;
//! - **every** local track is paired with a recording, and every pairing
//!   is itself confident in [`album_match`]'s sense. A title and an
//!   artist alone do not separate an album from its deluxe edition or
//!   its live version; a tracklist that agrees title by title and
//!   duration by duration does.
//!
//! Anything short of that but still plausible ([`DOUBTFUL_TOTAL`]) goes
//! to review. The thresholds were set on a real library: see
//! `docs/features/enrichment.md`.

use crate::metadata::album_match::{self, Assignment, Confidence, TrackSignals, UNKNOWN_SCORE};
use crate::metadata::name_match::{names_match, normalize_name};

/// Album titles at or above this agree.
pub const CONFIDENT_TITLE: f64 = 0.90;
/// An artist score at or above this agrees: only the shared matcher's
/// yes reaches it.
pub const CONFIDENT_ARTIST: f64 = 0.90;
/// Below this total nothing is proposed at all.
pub const DOUBTFUL_TOTAL: f64 = 0.60;

/// Weights of the album score. The tracks weigh most: they are the one
/// signal an edition, a live album or a namesake cannot fake.
const W_TITLE: f64 = 0.30;
const W_ARTIST: f64 = 0.20;
const W_TRACKS: f64 = 0.50;

/// One side of the comparison: a local album, or a release.
#[derive(Debug, Clone, Default)]
pub struct AlbumSide {
    pub title: String,
    /// The album artist as displayed: "A; B" locally, the credit with
    /// its join phrases on a release.
    pub artist: Option<String>,
    /// The individual names behind `artist`, when known: the local split
    /// credit, or the release's credited artists.
    pub artist_names: Vec<String>,
    pub tracks: Vec<TrackSignals>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Applied without asking.
    Confident,
    /// Offered for review.
    Doubtful,
    /// Not this album.
    NoMatch,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AlbumScore {
    pub title: f64,
    pub artist: f64,
    /// The pairing's scores summed over the local tracks, divided by
    /// their number: an unpaired track counts zero.
    pub tracks: f64,
    pub local_tracks: usize,
    pub remote_tracks: usize,
    pub paired: usize,
    pub confident_pairs: usize,
    pub total: f64,
}

#[derive(Debug, Clone)]
pub struct AlbumMatch {
    pub score: AlbumScore,
    pub verdict: Verdict,
    /// Indices into the local and the remote track lists.
    pub assignments: Vec<Assignment>,
}

/// Compare a local album with a release.
pub fn match_album(local: &AlbumSide, remote: &AlbumSide) -> AlbumMatch {
    let title = album_title_similarity(&local.title, &remote.title);
    let artist = artist_similarity(local, remote);
    let assignments = album_match::assign(
        &without_featuring(&local.tracks),
        &without_featuring(&remote.tracks),
    );
    let local_tracks = local.tracks.len();
    let paired = assignments.len();
    let confident_pairs = assignments
        .iter()
        .filter(|a| a.confidence == Confidence::Confident)
        .count();
    let tracks = if local_tracks == 0 {
        0.0
    } else {
        assignments.iter().map(|a| a.score).sum::<f64>() / local_tracks as f64
    };
    let total = (W_TITLE * title + W_ARTIST * artist + W_TRACKS * tracks).clamp(0.0, 1.0);
    let score = AlbumScore {
        title,
        artist,
        tracks,
        local_tracks,
        remote_tracks: remote.tracks.len(),
        paired,
        confident_pairs,
        total,
    };
    let verdict = verdict(&score);
    AlbumMatch {
        score,
        verdict,
        assignments,
    }
}

/// The verdict a score earns. See the module documentation.
pub fn verdict(score: &AlbumScore) -> Verdict {
    if score.local_tracks == 0 || score.paired == 0 {
        return Verdict::NoMatch;
    }
    let every_track_sure =
        score.paired == score.local_tracks && score.confident_pairs == score.local_tracks;
    if score.title >= CONFIDENT_TITLE && score.artist >= CONFIDENT_ARTIST && every_track_sure {
        return Verdict::Confident;
    }
    if score.total >= DOUBTFUL_TOTAL {
        Verdict::Doubtful
    } else {
        Verdict::NoMatch
    }
}

/// How well a search hit's summary fits, before its tracklist is fetched:
/// enough to choose which hits are worth a lookup, each of which costs a
/// second of the service's budget.
pub fn prescore(local: &AlbumSide, title: &str, artist: &str, track_count: Option<i64>) -> f64 {
    let remote = AlbumSide {
        title: title.to_owned(),
        artist: Some(artist.to_owned()),
        artist_names: vec![artist.to_owned()],
        tracks: Vec::new(),
    };
    let count = match track_count {
        Some(n) if n > 0 => {
            let local = local.tracks.len() as i64;
            if local == n {
                1.0
            } else if local < n {
                // A partial album is common: the user kept the songs
                // they wanted.
                0.7
            } else {
                (1.0 - (local - n) as f64 / local as f64).max(0.0)
            }
        }
        _ => UNKNOWN_SCORE,
    };
    0.45 * album_title_similarity(&local.title, title)
        + 0.35 * artist_similarity(local, &remote)
        + 0.20 * count
}

/// Title agreement, with an edition suffix set aside: the best of the
/// titles as written and the titles cut before their first bracket or a
/// trailing " - Single" / " - EP".
pub fn album_title_similarity(a: &str, b: &str) -> f64 {
    let raw = album_match::title_similarity(a, b);
    let (sa, sb) = (strip_edition(a), strip_edition(b));
    if sa == a && sb == b {
        return raw;
    }
    raw.max(album_match::title_similarity(sa, sb))
}

/// The title before its edition suffix, or the title itself when nothing
/// would be left.
pub fn strip_edition(title: &str) -> &str {
    let mut cut = title.trim();
    if let Some(at) = cut.find(['(', '[']) {
        let head = cut[..at].trim_end();
        if !head.is_empty() {
            cut = head;
        }
    }
    for suffix in [" - single", " - ep"] {
        let lower = cut.to_lowercase();
        if lower.ends_with(suffix) && lower.len() > suffix.len() {
            // `to_lowercase` can change byte lengths; cut at the char
            // count the suffix occupies rather than at its byte length.
            let keep = cut.chars().count() - suffix.chars().count();
            let end = cut.char_indices().nth(keep).map_or(cut.len(), |(i, _)| i);
            cut = cut[..end].trim_end();
        }
    }
    cut
}

/// The tracks with a guest credit cut from their titles, for pairing.
///
/// A file tagged "Waterfalls (feat. Sam Harper & Bobby Harvey)" and a
/// catalogue that credits the guests on the artist line and titles the
/// song "Waterfalls" are one recording, and the edit distance between
/// the two titles is most of the longer one. Only a guest credit is cut:
/// a "(Remix)" or a "(Live)" names a different recording and stays.
fn without_featuring(tracks: &[TrackSignals]) -> Vec<TrackSignals> {
    tracks
        .iter()
        .map(|t| TrackSignals {
            title: strip_featuring(&t.title).to_owned(),
            ..t.clone()
        })
        .collect()
}

/// The title before a guest credit — "(feat. …)", "[ft. …]", "(with …)",
/// or a bare " feat. …" — or the title itself when nothing would be left.
pub fn strip_featuring(title: &str) -> &str {
    const MARKERS: [&str; 8] = [
        "(feat.", "(feat ", "(ft.", "(with ", "[feat.", "[feat ", "[ft.", " feat. ",
    ];
    let lower = title.to_lowercase();
    // Byte offsets of `lower` are only valid in `title` while lowering
    // kept every length; a title it did not is left whole rather than
    // cut at the wrong place.
    if lower.len() != title.len() {
        return title.trim();
    }
    let cut = MARKERS
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .map(|at| title[..at].trim_end())
        .filter(|head| !head.is_empty());
    cut.unwrap_or(title.trim())
}

/// Artist agreement in `0.0..=1.0`.
///
/// 1.0 when the shared matcher accepts the displayed credits; 0.95 when
/// it accepts one local name against one credited name — "A; B" against
/// "A & B", where the joiners differ but the people are the same; and
/// otherwise the edit-distance similarity of the normalized credits,
/// capped below [`CONFIDENT_ARTIST`] so a near-miss spelling is never
/// confident. Half when the local album names no artist: unknown, not
/// against.
pub fn artist_similarity(local: &AlbumSide, remote: &AlbumSide) -> f64 {
    let (Some(la), Some(ra)) = (local.artist.as_deref(), remote.artist.as_deref()) else {
        return UNKNOWN_SCORE;
    };
    if normalize_name(la).is_empty() || normalize_name(ra).is_empty() {
        return UNKNOWN_SCORE;
    }
    if names_match(la, ra) {
        return 1.0;
    }
    let any_pair = local.artist_names.iter().any(|l| {
        remote
            .artist_names
            .iter()
            .any(|r| names_match(l.as_str(), r.as_str()))
    });
    if any_pair {
        return 0.95;
    }
    album_match::title_similarity(la, ra).min(CONFIDENT_ARTIST - 0.05)
}

#[cfg(test)]
mod tests;
