use super::*;

/// The case the issue singles out: the outer pair is 3 s apart, so a
/// pairwise rule would split one recording into two groups.
#[test]
fn chains_transitively_across_a_gap_wider_than_the_tolerance() {
    let items = [(1, 180_000), (2, 181_500), (3, 183_000)];
    assert_eq!(
        chain_by_duration(&items, DURATION_TOLERANCE_MS),
        vec![vec![1, 2, 3]]
    );
}

#[test]
fn splits_when_a_step_exceeds_the_tolerance() {
    let items = [(1, 180_000), (2, 181_000), (3, 190_000), (4, 190_500)];
    assert_eq!(
        chain_by_duration(&items, DURATION_TOLERANCE_MS),
        vec![vec![1, 2], vec![3, 4]]
    );
}

/// A track that matches nothing is not a duplicate. It must not
/// appear as a group of one, and — the part that is easy to get
/// wrong — it must not swallow the group that follows it either.
#[test]
fn a_lone_track_between_two_groups_is_dropped() {
    let items = [
        (1, 100_000),
        (2, 100_500),
        (3, 150_000),
        (4, 200_000),
        (5, 200_400),
    ];
    assert_eq!(
        chain_by_duration(&items, DURATION_TOLERANCE_MS),
        vec![vec![1, 2], vec![4, 5]]
    );
}

#[test]
fn fewer_than_two_tracks_is_never_a_group() {
    assert!(chain_by_duration(&[], DURATION_TOLERANCE_MS).is_empty());
    assert!(chain_by_duration(&[(1, 180_000)], DURATION_TOLERANCE_MS).is_empty());
}

/// Input order must not change the answer — the query feeding this
/// has no ORDER BY worth trusting, and a list that reshuffles
/// between two runs is a list nobody believes.
#[test]
fn the_result_does_not_depend_on_input_order() {
    let forward = [(1, 180_000), (2, 181_500), (3, 183_000), (4, 300_000)];
    let mut backward = forward;
    backward.reverse();
    assert_eq!(
        chain_by_duration(&forward, DURATION_TOLERANCE_MS),
        chain_by_duration(&backward, DURATION_TOLERANCE_MS)
    );
}

/// Equal durations are one group, and ties break on id so the
/// contents are stable.
#[test]
fn identical_durations_group_in_id_order() {
    let items = [(7, 200_000), (3, 200_000), (5, 200_000)];
    assert_eq!(
        chain_by_duration(&items, DURATION_TOLERANCE_MS),
        vec![vec![3, 5, 7]]
    );
}

/// A zero tolerance still groups exact matches — the degenerate
/// setting has to mean "byte-identical durations", not "nothing".
#[test]
fn zero_tolerance_groups_only_exact_matches() {
    let items = [(1, 180_000), (2, 180_000), (3, 180_001)];
    assert_eq!(chain_by_duration(&items, 0), vec![vec![1, 2]]);
}
