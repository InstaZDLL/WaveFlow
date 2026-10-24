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
//!    so it is ignored rather than trusted. On an album of several discs,
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
            disc.total = Some(disc.total.map_or(total, |t: i64| t.max(total)));
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
mod tests {
    use super::*;

    fn t(disc: Option<i64>, number: Option<i64>) -> NumberedTrack {
        NumberedTrack {
            disc_number: disc,
            track_number: number,
            track_total: None,
            disc_total: None,
        }
    }

    fn of_total(disc: Option<i64>, number: i64, total: i64) -> NumberedTrack {
        NumberedTrack {
            track_total: Some(total),
            ..t(disc, Some(number))
        }
    }

    fn missing(c: &AlbumCompleteness) -> Vec<(i64, Vec<i64>)> {
        c.discs.iter().map(|d| (d.disc, d.tracks.clone())).collect()
    }

    #[test]
    fn a_complete_album_is_complete() {
        let album: Vec<_> = (1..=10).map(|n| of_total(None, n, 10)).collect();
        assert!(album_completeness(&album).is_complete());
    }

    /// The case the card is about: the numbers alone run 2-10 without a
    /// hole, and only the convention that a disc starts at 1 sees it.
    #[test]
    fn a_missing_opener_is_seen_without_a_total() {
        let album: Vec<_> = (2..=10).map(|n| t(None, Some(n))).collect();
        let c = album_completeness(&album);
        assert_eq!(missing(&c), [(1, vec![1])]);
        assert!(c.missing_at_the_ends());
    }

    /// And the closer needs the total: 1-9 of 10.
    #[test]
    fn a_missing_closer_needs_the_total() {
        let without: Vec<_> = (1..=9).map(|n| t(None, Some(n))).collect();
        assert!(album_completeness(&without).is_complete());
        let with: Vec<_> = (1..=9).map(|n| of_total(None, n, 10)).collect();
        let c = album_completeness(&with);
        assert_eq!(missing(&c), [(1, vec![10])]);
        assert!(c.missing_at_the_ends());
    }

    #[test]
    fn a_hole_in_the_middle_is_reported_but_not_at_the_ends() {
        let album: Vec<_> = [1, 2, 3, 5, 6]
            .iter()
            .map(|n| of_total(None, *n, 6))
            .collect();
        let c = album_completeness(&album);
        assert_eq!(missing(&c), [(1, vec![4])]);
        assert!(!c.missing_at_the_ends());
    }

    #[test]
    fn an_album_without_numbers_is_not_judged() {
        let album = [t(None, None), t(None, None), t(None, None)];
        assert!(album_completeness(&album).is_complete());
    }

    /// The unnumbered track may be the very one the numbers miss.
    #[test]
    fn one_unnumbered_track_leaves_its_disc_unjudged() {
        let album = [t(None, Some(2)), t(None, Some(3)), t(None, None)];
        assert!(album_completeness(&album).is_complete());
    }

    /// A song kept from an album is not a broken album.
    #[test]
    fn a_lone_track_is_not_judged() {
        assert!(album_completeness(&[of_total(None, 5, 12)]).is_complete());
        assert!(album_completeness(&[of_total(Some(2), 5, 12)]).is_complete());
    }

    /// Three songs of a twelve-track album: more missing than present.
    #[test]
    fn a_few_songs_kept_from_a_long_album_are_not_judged() {
        let album = [
            of_total(None, 2, 12),
            of_total(None, 5, 12),
            of_total(None, 7, 12),
        ];
        assert!(album_completeness(&album).is_complete());
    }

    /// "101, 102, 103" — disc and track in one number.
    #[test]
    fn numbers_that_are_not_positions_are_not_judged() {
        let album: Vec<_> = (101..=103).map(|n| t(None, Some(n))).collect();
        assert!(album_completeness(&album).is_complete());
    }

    /// The box set the inventory's gap rule was written for: disc 2
    /// continues where disc 1 stopped.
    #[test]
    fn continuous_numbering_across_discs_is_recognised() {
        let mut album: Vec<_> = (1..=10).map(|n| t(Some(1), Some(n))).collect();
        album.extend((11..=20).map(|n| of_total(Some(2), n, 10)));
        assert!(album_completeness(&album).is_complete());
        // And a hole in it is still one.
        album.retain(|x| x.track_number != Some(20));
        let c = album_completeness(&album);
        assert!(c.is_complete(), "no total to say 20 existed: {c:?}");
    }

    #[test]
    fn each_disc_is_judged_on_its_own() {
        let mut album: Vec<_> = (1..=8).map(|n| of_total(Some(1), n, 8)).collect();
        album.extend((2..=6).map(|n| of_total(Some(2), n, 6)));
        let c = album_completeness(&album);
        assert_eq!(missing(&c), [(2, vec![1])]);
    }

    #[test]
    fn a_missing_disc_is_reported() {
        let mut album: Vec<_> = (1..=4).map(|n| t(Some(1), Some(n))).collect();
        for x in &mut album {
            x.disc_total = Some(2);
        }
        let c = album_completeness(&album);
        assert_eq!(c.missing_discs, [2]);
        assert!(c.missing_at_the_ends());
        // Present on both sides: disc 2 of 3.
        let mut album: Vec<_> = (1..=4).map(|n| t(Some(1), Some(n))).collect();
        album.extend((1..=4).map(|n| t(Some(3), Some(n))));
        assert_eq!(album_completeness(&album).missing_discs, [2]);
    }

    #[test]
    fn a_total_below_the_numbers_is_not_trusted() {
        let album: Vec<_> = (1..=12).map(|n| of_total(None, n, 10)).collect();
        assert!(album_completeness(&album).is_complete());
    }

    /// A box set whose files carry the album's total on every track, with
    /// continuous numbering or per-disc numbering alike: complete.
    #[test]
    fn an_album_wide_total_is_not_a_disc_total() {
        let mut continuous: Vec<_> = (1..=10).map(|n| of_total(Some(1), n, 20)).collect();
        continuous.extend((11..=20).map(|n| of_total(Some(2), n, 20)));
        assert!(album_completeness(&continuous).is_complete());
        let mut per_disc: Vec<_> = (1..=10).map(|n| of_total(Some(1), n, 20)).collect();
        per_disc.extend((1..=10).map(|n| of_total(Some(2), n, 20)));
        assert!(album_completeness(&per_disc).is_complete());
        // While a per-disc total still finds a missing closer.
        let mut short: Vec<_> = (1..=9).map(|n| of_total(Some(1), n, 10)).collect();
        short.extend((1..=10).map(|n| of_total(Some(2), n, 10)));
        assert_eq!(missing(&album_completeness(&short)), [(1, vec![10])]);
    }

    /// Two songs kept from disc 2 of a double album do not make disc 1
    /// "missing".
    #[test]
    fn a_disc_that_fails_rule_five_does_not_judge_the_discs() {
        let mut a = of_total(Some(2), 3, 12);
        a.disc_total = Some(2);
        let mut b = of_total(Some(2), 7, 12);
        b.disc_total = Some(2);
        assert!(album_completeness(&[a, b]).is_complete());
    }

    /// Continuous numbering with the disc before absent: its numbers are
    /// that disc's, reported as the missing disc and nothing else.
    #[test]
    fn a_disc_after_a_missing_one_is_not_judged_at_its_start() {
        let mut album: Vec<_> = (11..=20).map(|n| t(Some(2), Some(n))).collect();
        album.extend((21..=30).map(|n| t(Some(3), Some(n))));
        let c = album_completeness(&album);
        assert!(c.discs.is_empty(), "{c:?}");
        assert_eq!(c.missing_discs, [1]);

        let mut album: Vec<_> = (1..=10).map(|n| t(Some(1), Some(n))).collect();
        album.extend((21..=30).map(|n| t(Some(3), Some(n))));
        let c = album_completeness(&album);
        assert!(c.discs.is_empty(), "{c:?}");
        assert_eq!(c.missing_discs, [2]);
    }

    /// A disc number no release has — the range of missing discs would
    /// otherwise run to it.
    #[test]
    fn an_absurd_disc_number_reads_as_none() {
        let album = [
            t(Some(4_000_000_000), Some(1)),
            t(Some(4_000_000_000), Some(2)),
        ];
        assert!(album_completeness(&album).is_complete());
    }

    #[test]
    fn duplicate_numbers_count_once() {
        let album = [
            t(None, Some(1)),
            t(None, Some(2)),
            t(None, Some(2)),
            t(None, Some(4)),
        ];
        assert_eq!(missing(&album_completeness(&album)), [(1, vec![3])]);
    }
}
