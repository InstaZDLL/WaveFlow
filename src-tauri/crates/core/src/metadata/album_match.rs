//! Matching a local album's tracks against a catalogue's (#599).
//!
//! Matching the *album* is the easy half — a title and an artist go to
//! a search endpoint and the first plausible hit is usually right.
//! Matching the tracks inside it is where this kind of feature fails,
//! and it fails in two specific ways that this module is shaped
//! against:
//!
//! 1. **Judging on one signal.** A title alone cannot separate the two
//!    "Intro"s on a record; a duration alone cannot tell two three-
//!    minute songs apart. Three weighted signals are used instead, and
//!    the weights are deliberately unequal — the title carries most of
//!    the identity, the duration confirms it, and the track number is
//!    corroboration from a field that is wrong often enough to trust
//!    least.
//! 2. **Choosing per track.** Asking "what is the best remote track
//!    for this local one" lets a generic title win against several
//!    local tracks at once and capture one that belonged to another.
//!    The assignment here is global and greedy: every pair is scored,
//!    the best pair wins, and both sides are consumed.
//!
//! Nothing here touches the network or the database, which is what
//! makes the rules above testable as arithmetic.

use super::name_match::normalize_name;

/// Weights of the three signals. They sum to 1.
const W_TITLE: f64 = 0.60;
const W_DURATION: f64 = 0.25;
const W_NUMBER: f64 = 0.15;

/// What a signal scores when one side has no value for it.
///
/// **Not zero.** A track with no number is not evidence *against* a
/// match; scoring it zero would push every untagged file below the
/// threshold and make the feature useless on exactly the libraries
/// that need it most — the badly tagged ones.
pub const UNKNOWN_SCORE: f64 = 0.5;

/// Above this, the match is offered as settled.
pub const CONFIDENT: f64 = 0.85;
/// Above this but below [`CONFIDENT`], the match is offered for review.
/// Below it there is no match at all.
pub const DOUBTFUL: f64 = 0.55;

/// How far apart two durations may be before the signal is worthless.
///
/// Ten seconds: longer than the gap between a catalogue's rounding and
/// a file's real length, shorter than the gap between two different
/// songs of about the same size.
const DURATION_TOLERANCE_MS: f64 = 10_000.0;

/// One side of a comparison — a local file or a catalogue entry.
#[derive(Debug, Clone, Default)]
pub struct TrackSignals {
    pub title: String,
    pub duration_ms: Option<i64>,
    pub track_number: Option<i64>,
}

/// How sure the assignment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Confident,
    Doubtful,
}

/// One local track paired with one catalogue entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// Index into the local slice.
    pub local: usize,
    /// Index into the remote slice.
    pub remote: usize,
    pub score: f64,
    pub confidence: Confidence,
}

/// How well two tracks agree, in `0.0..=1.0`.
pub fn score(local: &TrackSignals, remote: &TrackSignals) -> f64 {
    let title = title_similarity(&local.title, &remote.title);
    let duration = duration_similarity(local.duration_ms, remote.duration_ms);
    let number = number_similarity(local.track_number, remote.track_number);
    (W_TITLE * title + W_DURATION * duration + W_NUMBER * number).clamp(0.0, 1.0)
}

/// Pair local tracks with catalogue entries, best pair first.
///
/// Each side is consumed once, so a catalogue entry cannot be handed to
/// two local files however well it scores against both — the second
/// file keeps looking, which is exactly what stops a generic title from
/// swallowing a record.
///
/// Pairs scoring below [`DOUBTFUL`] are not returned: a local track
/// with no match is a real answer, and inventing one for it is how this
/// kind of feature corrupts a library.
pub fn assign(locals: &[TrackSignals], remotes: &[TrackSignals]) -> Vec<Assignment> {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::with_capacity(locals.len() * remotes.len());
    for (li, local) in locals.iter().enumerate() {
        for (ri, remote) in remotes.iter().enumerate() {
            let s = score(local, remote);
            if s >= DOUBTFUL {
                pairs.push((s, li, ri));
            }
        }
    }
    // Descending score; the indices break ties so the result is a
    // function of its inputs rather than of the sort's stability.
    pairs.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });

    let mut local_taken = vec![false; locals.len()];
    let mut remote_taken = vec![false; remotes.len()];
    let mut out = Vec::new();
    for (s, li, ri) in pairs {
        if local_taken[li] || remote_taken[ri] {
            continue;
        }
        local_taken[li] = true;
        remote_taken[ri] = true;
        out.push(Assignment {
            local: li,
            remote: ri,
            score: s,
            confidence: if s >= CONFIDENT {
                Confidence::Confident
            } else {
                Confidence::Doubtful
            },
        });
    }
    // Back into the album's own order, which is how the review screen
    // reads: a list that jumps around by score is a list nobody can
    // check against the record in front of them.
    out.sort_by_key(|a| a.local);
    out
}

/// Title agreement, over [`normalize_name`].
///
/// The normaliser is the shared one on purpose: it folds NFD combining
/// marks, so a library tagged `Bjo\u{308}rk` matches a catalogue's
/// `Björk`. A transliteration table written for this feature would not,
/// and accented titles are not an edge case in a music library.
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let a = normalize_name(a);
    let b = normalize_name(b);
    if a.is_empty() || b.is_empty() {
        return UNKNOWN_SCORE;
    }
    if a == b {
        return 1.0;
    }
    let distance = levenshtein(&a, &b) as f64;
    let longest = a.chars().count().max(b.chars().count()) as f64;
    (1.0 - distance / longest).clamp(0.0, 1.0)
}

fn duration_similarity(a: Option<i64>, b: Option<i64>) -> f64 {
    let (Some(a), Some(b)) = (a, b) else {
        return UNKNOWN_SCORE;
    };
    let gap = (a - b).abs() as f64;
    (1.0 - gap / DURATION_TOLERANCE_MS).clamp(0.0, 1.0)
}

/// A track number agrees or it does not — there is no near miss. Track
/// 4 is not "almost" track 5; it is a different song.
fn number_similarity(a: Option<i64>, b: Option<i64>) -> f64 {
    match (a, b) {
        (Some(a), Some(b)) if a == b => 1.0,
        (Some(_), Some(_)) => 0.0,
        _ => UNKNOWN_SCORE,
    }
}

/// Edit distance over characters, two rows at a time.
///
/// Titles are short and an album is a few dozen of them, so the
/// quadratic cost is a few thousand character comparisons — far below
/// the network call that fetched the catalogue side.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, duration_ms: Option<i64>, track_number: Option<i64>) -> TrackSignals {
        TrackSignals {
            title: title.to_string(),
            duration_ms,
            track_number,
        }
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
}
