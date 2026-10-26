//! Which tracks an album is missing, disc by disc.
//!
//! The library already reports a hole *inside* a disc's numbering (the
//! inventory's "gap" category, `MAX - MIN + 1 > COUNT(DISTINCT)`), and
//! says why it stops there: numbering alone cannot tell a nine-track
//! album from a ten-track album missing its opener or its closer. The
//! track total a file carries (`TRCK` "4/12", `TRACKTOTAL`, `trkn`) can,
//! and so can the convention that a disc starts at 1 — which is the part
//! that has to be handled with care, because it is not always true.
//!
//! # The rule
//!
//! Per disc (a track with no disc number is on disc 1):
//!
//! 1. **Every track must be numbered.** One unnumbered track and the
//!    disc is not judged: it may be the very track the numbers say is
//!    missing.
//! 2. **At least two distinct numbers.** A lone track is a song kept
//!    from an album, not a broken album.
//! 3. **Where the disc starts.** At 1 — unless the album numbers its
//!    discs continuously (a box set whose disc 2 runs 11-20 after a disc
//!    1 of 1-10), recognised by the disc numbering on past the highest
//!    number of the disc before it: it then starts right after that one.
//!    A disc whose previous disc is absent is not judged at its start —
//!    under continuous numbering its first numbers belong to the missing
//!    disc, which is reported as such.
//! 4. **Where it ends.** At the highest of the numbers present and the
//!    track total the files carry. A total *below* the highest number is
//!    a contradiction (a per-disc total on a continuously numbered set),
//!    so it is ignored rather than trusted, and so are totals the tracks of
//!    one disc disagree on. On an album of several discs,
//!    a total that is the album's rather than the disc's is ignored too:
//!    one above every number on the album, or equal to the count of
//!    tracks across all of its discs.
//! 5. **At least half of it present.** A disc missing more tracks than it
//!    holds is not judged: that is a handful of songs kept from a long
//!    album, or numbers that are not positions at all ("101" for disc 1,
//!    track 1) — and a list of ninety-nine "missing" tracks is noise.
//!
//! Missing discs follow the same shape one level up: discs 1 to the
//! highest of the disc numbers and the disc total, minus the discs
//! present, judged only when at least half of them are there and at
//! least one disc passed rules 1, 2 and 5.
//!
//! An album with no numbers at all is never judged, and neither is one
//! whose only disc fails rule 1, 2 or 5: "not judged" means "nothing
//! reported", never "incomplete".

use std::collections::{BTreeMap, BTreeSet};

/// One track of the album, as the completeness check reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumberedTrack {
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    /// The track total the file carries — the size of *its disc*.
    pub track_total: Option<i64>,
    pub disc_total: Option<i64>,
}

/// The tracks a disc is missing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MissingOnDisc {
    pub disc: i64,
    pub tracks: Vec<i64>,
    /// Whether the missing tracks include the disc's first or last —
    /// what the inventory's gap category cannot see on its own.
    pub at_the_ends: bool,
}

/// What an album is missing. Empty when it is complete or not judged.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct AlbumCompleteness {
    pub discs: Vec<MissingOnDisc>,
    pub missing_discs: Vec<i64>,
}

impl AlbumCompleteness {
    pub fn is_complete(&self) -> bool {
        self.discs.is_empty() && self.missing_discs.is_empty()
    }

    /// Whether a first or last track (or a whole disc) is missing.
    pub fn missing_at_the_ends(&self) -> bool {
        !self.missing_discs.is_empty() || self.discs.iter().any(|d| d.at_the_ends)
    }
}

/// Numbers beyond this are not track positions; see rule 5.
const MAX_POSITION: i64 = 9_999;

/// Judge one album from its tracks (available ones, in any order).
pub fn album_completeness(tracks: &[NumberedTrack]) -> AlbumCompleteness {
    #[derive(Default)]
    struct Disc {
        numbers: BTreeSet<i64>,
        unnumbered: bool,
        total: Option<i64>,
        /// Two tracks of the disc declare different totals: neither is
        /// a bound anyone can trust.
        total_conflict: bool,
    }

    let mut discs: BTreeMap<i64, Disc> = BTreeMap::new();
    let mut disc_total: Option<i64> = None;
    for track in tracks {
        // Out of range reads as no disc number: a tag can claim disc
        // four billion, and the missing-disc range below must stay small.
        let disc_no = track
            .disc_number
            .filter(|d| (1..=MAX_POSITION).contains(d))
            .unwrap_or(1);
        let disc = discs.entry(disc_no).or_default();
        match track
            .track_number
            .filter(|n| (1..=MAX_POSITION).contains(n))
        {
            Some(n) => {
                disc.numbers.insert(n);
            }
            None => disc.unnumbered = true,
        }
        if let Some(total) = track.track_total.filter(|t| (1..=MAX_POSITION).contains(t)) {
            match disc.total {
                Some(previous) if previous != total => {
                    disc.total = None;
                    disc.total_conflict = true;
                }
                _ if !disc.total_conflict => disc.total = Some(total),
                _ => {}
            }
        }
        if let Some(total) = track.disc_total.filter(|t| (1..=MAX_POSITION).contains(t)) {
            disc_total = Some(disc_total.map_or(total, |t: i64| t.max(total)));
        }
    }

    // What an album-wide total would look like (rule 4), on an album of
    // several discs.
    let several_discs = discs.len() > 1;
    let album_max = discs
        .values()
        .filter_map(|d| d.numbers.last().copied())
        .max()
        .unwrap_or(0);
    let album_count: usize = discs.values().map(|d| d.numbers.len()).sum();

    let mut out = AlbumCompleteness::default();
    let mut judged_a_disc = false;
    // The highest number of the disc just before, when it is present.
    let mut previous: Option<(i64, i64)> = None;
    for (&disc_no, disc) in &discs {
        let (Some(&first), Some(&last)) = (disc.numbers.first(), disc.numbers.last()) else {
            previous = None;
            continue;
        };
        // Rule 3: continuous numbering across discs, and no verdict on
        // the start of a disc whose previous disc is absent.
        let start = match previous {
            Some((prev_disc, prev_max)) if prev_disc + 1 == disc_no && first > prev_max => {
                prev_max + 1
            }
            Some((prev_disc, _)) if prev_disc + 1 == disc_no => 1,
            _ if disc_no > 1 => first,
            _ => 1,
        };
        previous = Some((disc_no, last));
        if disc.unnumbered || disc.numbers.len() < 2 {
            continue;
        }
        // Rule 4: a total that contradicts the numbers, or that counts
        // the whole album, is not trusted.
        let end = match disc.total {
            Some(total)
                if total >= last
                    && !(several_discs
                        && total > last
                        && (total > album_max || total as usize == album_count)) =>
            {
                total
            }
            _ => last,
        };
        // Rule 5, counted before anything is collected.
        let span = (end - start + 1).max(0) as usize;
        let in_span = disc.numbers.range(start..=end).count();
        let missing_count = span - in_span;
        if missing_count > disc.numbers.len() {
            continue;
        }
        judged_a_disc = true;
        if missing_count == 0 {
            continue;
        }
        let missing: Vec<i64> = (start..=end)
            .filter(|n| !disc.numbers.contains(n))
            .collect();
        let at_the_ends = missing.first() == Some(&start) || missing.last() == Some(&end);
        out.discs.push(MissingOnDisc {
            disc: disc_no,
            tracks: missing,
            at_the_ends,
        });
    }

    // Missing discs, judged only on an album with a disc that was judged
    // itself: a lone track from disc 2 — or two songs kept from it — say
    // nothing about disc 1.
    let present: BTreeSet<i64> = discs.keys().copied().collect();
    if let (true, Some(&highest)) = (judged_a_disc, present.last()) {
        let end = disc_total.map_or(highest, |t| t.max(highest));
        // Counted before anything is collected: every present disc lies
        // in 1..=end, so this is exactly how many are missing.
        let missing_count = end as usize - present.len();
        if missing_count <= present.len() {
            out.missing_discs = (1..=end).filter(|d| !present.contains(d)).collect();
        }
    }
    out
}

#[cfg(test)]
mod tests;
