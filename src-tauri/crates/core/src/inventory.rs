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
mod tests;
