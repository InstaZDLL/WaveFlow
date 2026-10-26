use super::*;

fn track(title: &str, duration_ms: Option<i64>, track_number: Option<i64>) -> TrackSignals {
    TrackSignals {
        title: title.to_string(),
        duration_ms,
        track_number,
        disc_number: None,
    }
}

fn on_disc(mut t: TrackSignals, disc: i64) -> TrackSignals {
    t.disc_number = Some(disc);
    t
}

/// The accented-title case the shared normaliser exists for: a
/// decomposed local tag and a precomposed catalogue title are the
/// same song.
#[test]
fn a_decomposed_title_matches_its_precomposed_twin() {
    assert_eq!(title_similarity("Bjo\u{308}rk", "Björk"), 1.0);
    assert_eq!(title_similarity("Céline", "Celine"), 1.0);
}

/// Missing data is neutral, not damning. Without this the feature
/// would be useless on the untagged libraries that need it.
#[test]
fn a_missing_track_number_does_not_sink_a_match() {
    let local = track("Villanelle", Some(212_000), None);
    let remote = track("Villanelle", Some(212_000), Some(3));
    let s = score(&local, &remote);
    assert!(
        s >= CONFIDENT,
        "a title and duration that agree must be enough: {s}"
    );
}

/// A number that disagrees is evidence, where a missing one is not.
#[test]
fn a_wrong_track_number_costs_more_than_a_missing_one() {
    let local = track("Villanelle", Some(212_000), Some(9));
    let missing = track("Villanelle", Some(212_000), None);
    let remote = track("Villanelle", Some(212_000), Some(3));
    assert!(score(&local, &remote) < score(&missing, &remote));
}

/// The failure this module is shaped against: two generic titles
/// where a per-track best match would hand the same remote entry to
/// both local files.
#[test]
fn a_generic_title_cannot_be_taken_twice() {
    let locals = vec![
        track("Intro", Some(60_000), Some(1)),
        track("Intro", Some(95_000), Some(7)),
    ];
    let remotes = vec![
        track("Intro", Some(60_000), Some(1)),
        track("Intro", Some(95_000), Some(7)),
    ];
    let out = assign(&locals, &remotes);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].remote, 0);
    assert_eq!(out[1].remote, 1, "the second file must get the other entry");
}

/// The duration is what separates them when the numbers are gone.
#[test]
fn duration_separates_identical_titles() {
    let locals = vec![
        track("Interlude", Some(45_000), None),
        track("Interlude", Some(180_000), None),
    ];
    let remotes = vec![
        track("Interlude", Some(179_000), None),
        track("Interlude", Some(46_000), None),
    ];
    let out = assign(&locals, &remotes);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].remote, 1, "45 s goes with 46 s");
    assert_eq!(out[1].remote, 0, "180 s goes with 179 s");
}

/// A local track with nothing like it is left unmatched rather than
/// paired with the least bad option.
#[test]
fn a_track_with_no_counterpart_stays_unmatched() {
    let locals = vec![
        track("Hidden Track", Some(30_000), Some(99)),
        track("Villanelle", Some(212_000), Some(3)),
    ];
    let remotes = vec![track("Villanelle", Some(212_000), Some(3))];
    let out = assign(&locals, &remotes);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].local, 1);
}

/// Two discs, one track number each: without the disc the number
/// signal says "match" for both pairings, and on a box set of live
/// takes the titles and durations are close enough that it decides.
#[test]
fn the_disc_separates_two_tracks_numbered_the_same() {
    // Identical on every other signal, so the disc is the only
    // thing that can tell them apart: with differing durations the
    // duration would separate them and this would pass whether or
    // not the disc were read at all.
    let locals = vec![
        on_disc(track("Improvisation", Some(400_000), Some(3)), 1),
        on_disc(track("Improvisation", Some(400_000), Some(3)), 2),
    ];
    let remotes = vec![
        on_disc(track("Improvisation", Some(400_000), Some(3)), 2),
        on_disc(track("Improvisation", Some(400_000), Some(3)), 1),
    ];
    let out = assign(&locals, &remotes);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].remote, 1, "disc 1 goes with disc 1");
    assert_eq!(out[1].remote, 0, "disc 2 goes with disc 2");
}

/// A disc on one side only is not a disagreement. Plenty of
/// releases number their discs where the local files do not, and
/// reading that as a mismatch would cost every one of them the
/// number signal.
#[test]
fn a_disc_on_one_side_only_costs_nothing() {
    let local = track("Villanelle", Some(212_000), Some(3));
    let remote = on_disc(track("Villanelle", Some(212_000), Some(3)), 2);
    assert_eq!(score(&local, &remote), score(&local, &local.clone()));
}

/// The middle band is the reason the review screen exists: a match
/// good enough to offer, not good enough to apply unseen.
#[test]
fn a_near_miss_is_doubtful_rather_than_absent() {
    let local = track("Villanelle (Remastered)", Some(212_000), Some(3));
    let remote = track("Villanelle", Some(214_000), Some(3));
    let out = assign(&[local], &[remote]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].confidence, Confidence::Doubtful);
}

/// Results come back in the album's order, not in score order.
#[test]
fn assignments_are_returned_in_local_order() {
    let locals = vec![
        track("Bad Match", Some(100_000), Some(1)),
        track("Exact", Some(200_000), Some(2)),
    ];
    let remotes = vec![
        track("Exact", Some(200_000), Some(2)),
        track("Bad Match Indeed", Some(101_000), Some(1)),
    ];
    let out = assign(&locals, &remotes);
    assert_eq!(out.len(), 2);
    assert!(out[0].local < out[1].local);
}

#[test]
fn levenshtein_counts_edits() {
    assert_eq!(levenshtein("kitten", "sitting"), 3);
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("abc", ""), 3);
    assert_eq!(levenshtein("same", "same"), 0);
}
