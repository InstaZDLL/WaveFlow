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

/// At or above this, the match is offered as settled.
pub const CONFIDENT: f64 = 0.85;
/// At or above this but below [`CONFIDENT`], the match is offered for
/// review. Below it there is no match at all.
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
    /// The disc it sits on, where the release has more than one.
    ///
    /// A track number alone cannot separate disc 1's third track from
    /// disc 2's: both are "3", and on a box set of live recordings the
    /// titles and the durations are close enough that the number is
    /// what the pairing turns on.
    pub disc_number: Option<i64>,
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
    let number = number_similarity(local, remote);
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
///
/// The disc joins the comparison **only when both sides carry one**. A
/// release where one side numbers its discs and the other does not is
/// common — and it is not disagreement, so it must not read as one;
/// the track number then answers alone, exactly as before. When both
/// have it, the pair is compared as a pair, which is the only way
/// "disc 2, track 3" stops looking identical to "disc 1, track 3".
fn number_similarity(local: &TrackSignals, remote: &TrackSignals) -> f64 {
    let (Some(a), Some(b)) = (local.track_number, remote.track_number) else {
        return UNKNOWN_SCORE;
    };
    if a != b {
        return 0.0;
    }
    match (local.disc_number, remote.disc_number) {
        (Some(x), Some(y)) if x != y => 0.0,
        _ => 1.0,
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
mod tests;
