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

/// Tracks 1-10, half saying "of 10" and half "of 12": no bound.
#[test]
fn totals_a_disc_disagrees_on_are_ignored() {
    let album: Vec<_> = (1..=10)
        .map(|n| of_total(None, n, if n % 2 == 0 { 12 } else { 10 }))
        .collect();
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
