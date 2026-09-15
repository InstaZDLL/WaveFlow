//! Grouping rules for the library's "needs attention" inventory (#589).
//!
//! The queries live app-side, next to the pool; what is here is the one
//! piece of real logic — deciding which tracks are *probably* the same
//! recording — because it is the detail that settles whether the feature
//! is useful or merely present, and because it is worth testing without
//! a database.

/// How far apart two durations can be and still describe one recording.
///
/// Two seconds: the spread you get between a rip and a re-encode of the
/// same track, or between two pressings, without reaching into
/// genuinely different edits.
pub const DURATION_TOLERANCE_MS: i64 = 2_000;

/// Group tracks that are probably the same recording, by **chaining**
/// rather than comparing pairs.
///
/// `items` is `(track_id, duration_ms)` for one title+artist, in any
/// order. Every returned group has at least two members; a track that
/// matches nothing is not a group of one.
///
/// # Why chaining, and not a pairwise tolerance
///
/// 180 s, 181.5 s and 183 s are one recording. The outer pair is 3 s
/// apart, so a naive "every pair within tolerance" rule splits them in
/// two — and which two depends on iteration order, so the same library
/// can produce different answers on different runs. Sorting and
/// chaining consecutive neighbours gives one group, deterministically.
///
/// The cost is that chaining is transitive by construction: a long
/// enough ladder of 1.9 s steps joins durations that are minutes apart.
/// That is the right trade for a *probable*-duplicate list a human
/// reviews — a group with one wrong member is a moment's reading, while
/// a split group is a duplicate the user never finds — and it is why
/// this list is offered as "probable" rather than acted on
/// automatically.
pub fn chain_by_duration(items: &[(i64, i64)], tolerance_ms: i64) -> Vec<Vec<i64>> {
    if items.len() < 2 {
        return Vec::new();
    }
    let mut sorted: Vec<(i64, i64)> = items.to_vec();
    // By duration, then by id: two tracks of exactly the same length
    // would otherwise be ordered by whatever the caller's query
    // happened to return, and the group contents would follow.
    sorted.sort_by_key(|(id, duration)| (*duration, *id));

    let mut groups: Vec<Vec<i64>> = Vec::new();
    let mut current: Vec<i64> = vec![sorted[0].0];
    let mut previous = sorted[0].1;

    for (id, duration) in sorted.iter().skip(1) {
        if duration - previous <= tolerance_ms {
            current.push(*id);
        } else {
            if current.len() > 1 {
                groups.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            current.push(*id);
        }
        previous = *duration;
    }
    if current.len() > 1 {
        groups.push(current);
    }
    groups
}

#[cfg(test)]
mod tests {
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
}
