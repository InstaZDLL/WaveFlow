//! What a mood is, and how well a track fits one (#616).
//!
//! Mood Radio used to be a tempo window and nothing else: a track
//! inside the window was drawn as readily as any other, so Focus was
//! as likely to open on 109 BPM as on 85, and the home tile's promise
//! of "tempo and energy" was carried by a loudness ceiling on two of
//! the five moods. Two moods could also return the same kind of list,
//! because Chill's window sat entirely inside Focus's.
//!
//! The fix is to stop treating the window as the answer. It stays as a
//! gate — a track far outside a mood's tempo is not that mood — and
//! everything inside it is then **ranked by how well it fits**, so the
//! forty tracks that play are the forty best of the pool rather than
//! the first forty drawn.
//!
//! # Why the scoring lives here
//!
//! It is arithmetic over four numbers, it decides what the user hears,
//! and none of it needs a database. Keeping it in `waveflow-core` is
//! what lets it be tested against values rather than against a
//! library — the tempo octave rule below is impossible to be sure of
//! any other way.

/// A mood, as a shape rather than a window.
///
/// `bpm_centre` is what the mood *is*; `bpm_min` / `bpm_max` are only
/// how far from it a track may sit and still be considered. The
/// loudness bounds work the same way: they gate, and the distance from
/// the preferred side ranks.
#[derive(Debug, Clone, Copy)]
pub struct MoodProfile {
    /// Inclusive tempo gate. `None` is unbounded on that side.
    pub bpm_min: Option<f64>,
    pub bpm_max: Option<f64>,
    /// The tempo this mood is built around — the peak of the ranking.
    pub bpm_centre: f64,
    /// Loudness ceiling in LUFS (a negative number): quieter than this.
    pub lufs_max: Option<f64>,
    /// Loudness floor in LUFS: louder than this.
    pub lufs_min: Option<f64>,
    /// A loudness **gate**, unlike the two above: a track measured
    /// louder than this is not considered at all. Only Sleep sets one.
    /// It dates from analysis version 1, whose tempo stuck to 60 BPM
    /// whenever it could not decide, so a slow window held a fifth of a
    /// library; [`MoodProfile::energy_ceiling`] now does most of that
    /// work, and this stays for rows not yet re-measured. Unmeasured
    /// loudness passes.
    pub lufs_ceiling: Option<f64>,
    /// Preferred energy (0 calm .. 1 intense, analysis version 2): the
    /// ranking's bounds, like `lufs_max` / `lufs_min`.
    pub energy_max: Option<f64>,
    pub energy_min: Option<f64>,
    /// Energy **gates**: a track measured past one is not considered.
    /// Energy survives loudness normalisation, which is what makes it
    /// the signal that keeps a dance record out of Sleep and a ballad
    /// out of Workout. Unmeasured energy passes.
    pub energy_ceiling: Option<f64>,
    pub energy_floor: Option<f64>,
    /// Genre words that suit the mood, matched case-insensitively as
    /// substrings of the track's genres. A **bonus only** — never a
    /// gate. Genre strings come from whatever tagger wrote the file,
    /// so absence proves nothing and a miss must cost nothing.
    pub genre_words: &'static [&'static str],
}

/// How much a tempo read at half or double speed is discounted.
///
/// Tempo estimators land an octave out often enough that a 170 BPM
/// track can be recorded as 85 — and a library where that happened is
/// a library where Focus quietly fills with drum'n'bass. Both
/// readings are therefore considered, and the corrected one is worth
/// less: it is a guess about a measurement, where the plain reading is
/// only a measurement.
pub const OCTAVE_PENALTY: f64 = 0.65;

/// Tempo confidence from which the reading is taken as it is.
///
/// Measured against a reference on a real library, analysis version 2
/// is right on 91% of the readings at or above this, and an octave out
/// on only 7%: reinterpreting those would move far more correct
/// readings than it rescues. Below it — and on rows from version 1,
/// which have no confidence — the octave readings stay open.
pub const TRUSTED_TEMPO: f64 = 0.4;

/// What an unmeasured value scores.
///
/// Neither the reward of a match nor the cost of a miss: a track whose
/// loudness nobody has measured is not evidence that it is loud. It
/// ranks below a track measured inside the mood and above one measured
/// outside it, which is the only honest ordering — and it is what
/// keeps an unanalysed library from returning an empty radio.
pub const UNKNOWN_SCORE: f64 = 0.5;

/// Weights of the four signals. Tempo carries the mood; energy confirms
/// it; loudness says little once a library is normalised, but is all a
/// row from analysis version 1 has; genre is corroboration from a field
/// nobody validates.
const W_TEMPO: f64 = 0.5;
const W_ENERGY: f64 = 0.3;
const W_LOUDNESS: f64 = 0.1;
const W_GENRE: f64 = 0.1;

/// The weights before analysis version 2, for a row that has neither a
/// confidence nor an energy yet. Under the new weights its energy would
/// be a flat 0.5 and its loudness — the only intensity signal it has —
/// would count for a third of what it did, so a library halfway through
/// its re-analysis would rank its unmeasured half on tempo alone.
const V1_W_TEMPO: f64 = 0.6;
const V1_W_LOUDNESS: f64 = 0.3;

/// One track, as the ranking sees it.
#[derive(Debug, Clone)]
pub struct MoodCandidate {
    pub track_id: i64,
    /// `track.primary_artist` is nullable, and the per-artist cap has
    /// to bucket the tracks that lost theirs together rather than
    /// treating each as its own artist.
    pub primary_artist: Option<i64>,
    pub bpm: f64,
    /// How far to trust `bpm`, 0 to 1. `None` on a row from analysis
    /// version 1, which is judged as before.
    pub bpm_confidence: Option<f64>,
    pub loudness_lufs: Option<f64>,
    /// 0 (calm) to 1 (intense); `None` before analysis version 2.
    pub energy: Option<f64>,
    /// The track's genres, already lower-cased and joined — the shape
    /// the query hands back.
    pub genres: Option<String>,
}

/// The tempo reading to judge a track by, and what that reading costs.
///
/// Returns the interpretation that best suits the profile: the plain
/// reading, or half or double it when the plain one falls outside the
/// gate and the corrected one does not. The second element is the
/// factor the tempo score is multiplied by.
///
/// A reading is only corrected **toward a bounded side**: halved when
/// the window has a floor, doubled when it has a ceiling. Toward an
/// open side the correction rescues nothing and admits everything —
/// Sleep has no floor, so halving let in every track up to twice its
/// ceiling, nine tenths of a library.
///
/// A reading trusted at [`TRUSTED_TEMPO`] or more is never corrected.
pub fn effective_bpm(profile: &MoodProfile, bpm: f64, confidence: Option<f64>) -> (f64, f64) {
    if bpm <= 0.0 {
        return (bpm, 1.0);
    }
    if in_window(profile, bpm) || is_trusted(confidence) {
        return (bpm, 1.0);
    }
    // Only when the plain reading is out of the window: a track that
    // already fits is never reinterpreted, however well the double of
    // it would score. Octave correction exists to rescue a
    // misestimated track, not to move a correct one.
    for candidate in octave_readings(profile, bpm) {
        if in_window(profile, candidate) {
            return (candidate, OCTAVE_PENALTY);
        }
    }
    (bpm, 1.0)
}

/// The corrected readings a profile accepts: double when the window
/// has a ceiling, half when it has a floor. See [`effective_bpm`].
fn octave_readings(profile: &MoodProfile, bpm: f64) -> impl Iterator<Item = f64> {
    [
        profile.bpm_max.map(|_| bpm * 2.0),
        profile.bpm_min.map(|_| bpm / 2.0),
    ]
    .into_iter()
    .flatten()
}

/// Whether a tempo is sure enough to be taken as read.
pub fn is_trusted(confidence: Option<f64>) -> bool {
    confidence.is_some_and(|c| c >= TRUSTED_TEMPO)
}

/// Does this measured energy pass the mood's energy gates? An
/// unmeasured one does: absence is not evidence.
pub fn passes_energy_gate(profile: &MoodProfile, energy: Option<f64>) -> bool {
    let Some(energy) = energy else {
        return true;
    };
    profile.energy_ceiling.map_or(true, |c| energy <= c)
        && profile.energy_floor.map_or(true, |f| energy >= f)
}

/// Does this measured loudness pass the mood's loudness gate? An
/// unmeasured one does: absence is not evidence.
pub fn passes_loudness_gate(profile: &MoodProfile, lufs: Option<f64>) -> bool {
    match (profile.lufs_ceiling, lufs) {
        (Some(ceiling), Some(lufs)) => lufs <= ceiling,
        _ => true,
    }
}

/// Does this tempo pass the mood's gate?
pub fn in_window(profile: &MoodProfile, bpm: f64) -> bool {
    // `map_or(true, …)` rather than `is_none_or`, which is stable only
    // since 1.82 — the workspace MSRV is 1.80, and the lint that would
    // have caught it only fires on code the build actually compiles.
    profile.bpm_min.map_or(true, |min| bpm >= min) && profile.bpm_max.map_or(true, |max| bpm <= max)
}

/// Does this tempo pass the gate under any reading the profile accepts
/// — plain, doubled or halved? This is what the candidate query has to
/// ask, or the octave correction below would never see the tracks it
/// exists to rescue.
pub fn in_window_any_octave(profile: &MoodProfile, bpm: f64, confidence: Option<f64>) -> bool {
    bpm > 0.0
        && (in_window(profile, bpm)
            || (!is_trusted(confidence)
                && octave_readings(profile, bpm).any(|b| in_window(profile, b))))
}

/// How well a track fits a mood, in `0.0..=1.0`.
///
/// A tempo read with little confidence scores towards [`UNKNOWN_SCORE`]
/// in proportion: a reading the estimator itself doubts is weak
/// evidence either way.
pub fn fit_score(profile: &MoodProfile, candidate: &MoodCandidate) -> f64 {
    let (bpm, octave_factor) = effective_bpm(profile, candidate.bpm, candidate.bpm_confidence);
    let measured = tempo_score(profile, bpm) * octave_factor;
    let tempo = match candidate.bpm_confidence {
        Some(c) => {
            let c = c.clamp(0.0, 1.0);
            c * measured + (1.0 - c) * UNKNOWN_SCORE
        }
        None => measured,
    };
    let loudness = loudness_score(profile, candidate.loudness_lufs);
    let genre = genre_score(profile, candidate.genres.as_deref());
    if candidate.bpm_confidence.is_none() && candidate.energy.is_none() {
        return (V1_W_TEMPO * tempo + V1_W_LOUDNESS * loudness + W_GENRE * genre).clamp(0.0, 1.0);
    }
    let energy = energy_score(profile, candidate.energy);
    (W_TEMPO * tempo + W_ENERGY * energy + W_LOUDNESS * loudness + W_GENRE * genre).clamp(0.0, 1.0)
}

/// Where a measured energy sits relative to the mood's preferred
/// bounds: 1 inside, falling to 0 over a quarter of the scale past
/// them. Unmeasured energy scores [`UNKNOWN_SCORE`].
fn energy_score(profile: &MoodProfile, energy: Option<f64>) -> f64 {
    let Some(energy) = energy else {
        return UNKNOWN_SCORE;
    };
    /// Overshoot that takes the score from 1 to 0.
    const RUN: f64 = 0.25;
    if profile.energy_max.is_none() && profile.energy_min.is_none() {
        return UNKNOWN_SCORE;
    }
    let mut overshoot: f64 = 0.0;
    if let Some(max) = profile.energy_max {
        overshoot = overshoot.max(energy - max);
    }
    if let Some(min) = profile.energy_min {
        overshoot = overshoot.max(min - energy);
    }
    (1.0 - overshoot.max(0.0) / RUN).clamp(0.0, 1.0)
}

/// Distance from the mood's centre, normalised by the half-width of
/// its window. A track at the centre scores 1, one at the edge scores
/// close to 0, and the fall is linear because nothing about a tempo
/// preference justifies a sharper curve.
///
/// **An unbounded side has no "too far".** Sleep has no floor, so a
/// 30 BPM drone is not a worse fit for it than one at its 52 BPM
/// centre — measuring the distance anyway scored the slowest tracks in
/// the library worst for the mood built on slowness.
fn tempo_score(profile: &MoodProfile, bpm: f64) -> f64 {
    if bpm < profile.bpm_centre && profile.bpm_min.is_none() {
        return 1.0;
    }
    if bpm > profile.bpm_centre && profile.bpm_max.is_none() {
        return 1.0;
    }
    let reach = half_width(profile);
    if reach <= 0.0 {
        return 1.0;
    }
    (1.0 - (bpm - profile.bpm_centre).abs() / reach).clamp(0.0, 1.0)
}

/// How far the centre sits from the furthest edge of the window. An
/// unbounded side is measured from the bounded one, so an open-ended
/// mood (Sleep has no floor) still has a scale to divide by.
fn half_width(profile: &MoodProfile) -> f64 {
    let below = profile.bpm_min.map(|min| profile.bpm_centre - min);
    let above = profile.bpm_max.map(|max| max - profile.bpm_centre);
    match (below, above) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => 0.0,
    }
}

/// Where a measured loudness sits relative to the mood's bounds.
///
/// A track inside both bounds scores 1; outside one of them the score
/// falls with the overshoot, over a fixed six-decibel run — beyond
/// that the track is simply the wrong loudness for the mood and scores
/// 0. Unmeasured loudness scores [`UNKNOWN_SCORE`].
fn loudness_score(profile: &MoodProfile, lufs: Option<f64>) -> f64 {
    let Some(lufs) = lufs else {
        return UNKNOWN_SCORE;
    };
    /// Decibels of overshoot that take the score from 1 to 0.
    const RUN: f64 = 6.0;
    let mut overshoot: f64 = 0.0;
    if let Some(max) = profile.lufs_max {
        overshoot = overshoot.max(lufs - max);
    }
    if let Some(min) = profile.lufs_min {
        overshoot = overshoot.max(min - lufs);
    }
    (1.0 - overshoot.max(0.0) / RUN).clamp(0.0, 1.0)
}

/// Whether any of the mood's words appear in the track's genres.
///
/// A miss scores [`UNKNOWN_SCORE`] rather than 0, for the same reason
/// as an unmeasured loudness: most libraries carry genres nobody
/// curated, and a mood that punished every unrecognised genre would
/// rank a correctly-tagged library worse than an untagged one.
fn genre_score(profile: &MoodProfile, genres: Option<&str>) -> f64 {
    if profile.genre_words.is_empty() {
        return UNKNOWN_SCORE;
    }
    let Some(genres) = genres.filter(|g| !g.trim().is_empty()) else {
        return UNKNOWN_SCORE;
    };
    let haystack = genres.to_lowercase();
    if profile.genre_words.iter().any(|w| haystack.contains(w)) {
        1.0
    } else {
        UNKNOWN_SCORE
    }
}

/// Rank a pool and take the best of it, one artist at a time.
///
/// The pool arrives in random order — that is what keeps two runs of
/// the same mood from being the same list — and this picks the best
/// fits out of it. The per-artist cap is applied while walking the
/// ranking, so the tracks it skips are that artist's *weakest*, not
/// whichever the draw happened to reach first.
pub fn rank_and_cap(
    profile: &MoodProfile,
    candidates: Vec<MoodCandidate>,
    target_len: usize,
    per_artist_cap: usize,
) -> Vec<i64> {
    let mut scored: Vec<(f64, MoodCandidate)> = candidates
        .into_iter()
        .map(|c| (fit_score(profile, &c), c))
        .collect();
    // Descending fit, with the track id as a tie-break so the order is
    // total: an unstable comparison over equal scores would make the
    // same pool produce different queues, which is untestable and, for
    // a user re-running a mood, inexplicable.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.track_id.cmp(&b.1.track_id))
    });

    let mut per_artist: std::collections::HashMap<Option<i64>, usize> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(target_len);
    for (_, c) in scored {
        if out.len() >= target_len {
            break;
        }
        let count = per_artist.entry(c.primary_artist).or_insert(0);
        if *count >= per_artist_cap {
            continue;
        }
        *count += 1;
        out.push(c.track_id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOCUS: MoodProfile = MoodProfile {
        bpm_min: Some(70.0),
        bpm_max: Some(105.0),
        bpm_centre: 85.0,
        lufs_max: Some(-14.0),
        lufs_min: None,
        lufs_ceiling: None,
        energy_max: None,
        energy_min: None,
        energy_ceiling: None,
        energy_floor: None,
        genre_words: &["ambient", "classical"],
    };

    fn candidate(track_id: i64, bpm: f64, lufs: Option<f64>) -> MoodCandidate {
        MoodCandidate {
            track_id,
            primary_artist: Some(1),
            bpm,
            bpm_confidence: None,
            loudness_lufs: lufs,
            energy: None,
            genres: None,
        }
    }

    /// The whole point of the change: inside one window, the track
    /// nearer the mood's centre wins. Before this, both were equally
    /// likely.
    #[test]
    fn a_track_nearer_the_centre_scores_higher() {
        let near = fit_score(&FOCUS, &candidate(1, 85.0, Some(-16.0)));
        let far = fit_score(&FOCUS, &candidate(2, 104.0, Some(-16.0)));
        assert!(near > far, "near={near} far={far}");
    }

    /// A tempo read an octave out is rescued, and discounted for it.
    #[test]
    fn an_octave_error_is_rescued_but_discounted() {
        let (bpm, factor) = effective_bpm(&FOCUS, 170.0, None);
        assert_eq!(bpm, 85.0, "170 must be read as 85 for a mood built on 85");
        assert_eq!(factor, OCTAVE_PENALTY);

        let honest = fit_score(&FOCUS, &candidate(1, 85.0, Some(-16.0)));
        let rescued = fit_score(&FOCUS, &candidate(2, 170.0, Some(-16.0)));
        assert!(
            rescued < honest,
            "a corrected reading must rank below a plain one: {rescued} vs {honest}"
        );
        assert!(rescued > 0.0, "but it must still be a candidate");
    }

    /// A track already inside the window is never reinterpreted, even
    /// when the double of it would land nearer the centre.
    #[test]
    fn a_tempo_already_in_the_window_is_left_alone() {
        let party = MoodProfile {
            bpm_min: Some(60.0),
            bpm_max: Some(140.0),
            bpm_centre: 124.0,
            lufs_max: None,
            lufs_min: None,
            lufs_ceiling: None,
            energy_max: None,
            energy_min: None,
            energy_ceiling: None,
            energy_floor: None,
            genre_words: &[],
        };
        let (bpm, factor) = effective_bpm(&party, 62.0, None);
        assert_eq!((bpm, factor), (62.0, 1.0));
    }

    /// Unmeasured loudness is neither a pass nor a fail. This is the
    /// defect the issue names: an unmeasured track used to satisfy the
    /// ceiling exactly as well as a measured quiet one.
    #[test]
    fn unmeasured_loudness_ranks_between_measured_ones() {
        let quiet = fit_score(&FOCUS, &candidate(1, 85.0, Some(-20.0)));
        let unknown = fit_score(&FOCUS, &candidate(2, 85.0, None));
        let loud = fit_score(&FOCUS, &candidate(3, 85.0, Some(-4.0)));
        assert!(quiet > unknown, "quiet={quiet} unknown={unknown}");
        assert!(unknown > loud, "unknown={unknown} loud={loud}");
    }

    /// A genre the mood does not name costs nothing, because most
    /// libraries carry genres nobody curated.
    #[test]
    fn an_unrecognised_genre_costs_nothing_a_missing_one_does_not_either() {
        let mut named = candidate(1, 85.0, Some(-16.0));
        named.genres = Some("ambient".into());
        let mut other = candidate(2, 85.0, Some(-16.0));
        other.genres = Some("death metal".into());
        let missing = candidate(3, 85.0, Some(-16.0));

        let named = fit_score(&FOCUS, &named);
        let other = fit_score(&FOCUS, &other);
        let missing = fit_score(&FOCUS, &missing);
        assert!(named > other, "a named genre is a bonus");
        assert_eq!(
            other, missing,
            "an unrecognised genre must cost exactly what no genre costs"
        );
    }

    /// The cap skips an artist's weakest tracks, not the ones the draw
    /// reached last — which is only true because the cap is applied to
    /// the ranking rather than to the pool.
    #[test]
    fn the_per_artist_cap_keeps_the_best_of_that_artist() {
        let mut far = candidate(10, 104.0, Some(-16.0));
        far.primary_artist = Some(7);
        let mut near = candidate(11, 85.0, Some(-16.0));
        near.primary_artist = Some(7);
        let mut other = candidate(12, 86.0, Some(-16.0));
        other.primary_artist = Some(8);

        let picked = rank_and_cap(&FOCUS, vec![far, near, other], 10, 1);
        assert!(picked.contains(&11), "the artist's best must be kept");
        assert!(
            !picked.contains(&10),
            "its weaker track must be the one cut"
        );
        assert!(picked.contains(&12), "another artist is unaffected");
    }

    /// A mood open on one side does not punish tracks that go further
    /// that way. Sleep is the case: it has no floor, and scoring by
    /// distance from its centre made the slowest track in the library
    /// the worst fit for the mood built on slowness.
    const SLEEP: MoodProfile = MoodProfile {
        bpm_min: None,
        bpm_max: Some(68.0),
        bpm_centre: 52.0,
        lufs_max: Some(-18.0),
        lufs_min: None,
        lufs_ceiling: Some(-12.0),
        energy_max: None,
        energy_min: None,
        energy_ceiling: None,
        energy_floor: None,
        genre_words: &[],
    };

    #[test]
    fn an_unbounded_side_has_no_too_far() {
        let centre = fit_score(&SLEEP, &candidate(1, 52.0, Some(-20.0)));
        let slower = fit_score(&SLEEP, &candidate(2, 30.0, Some(-20.0)));
        assert_eq!(
            slower, centre,
            "below the centre, with no floor, is as good"
        );
        let edge = fit_score(&SLEEP, &candidate(3, 68.0, Some(-20.0)));
        assert!(
            edge < centre,
            "the bounded side still ranks: {edge} vs {centre}"
        );
    }

    /// Tracks that lost their artist share one bucket, rather than
    /// each counting as a different artist and filling the queue.
    #[test]
    fn artistless_tracks_share_one_bucket() {
        let mut a = candidate(1, 85.0, None);
        a.primary_artist = None;
        let mut b = candidate(2, 86.0, None);
        b.primary_artist = None;
        assert_eq!(rank_and_cap(&FOCUS, vec![a, b], 10, 1).len(), 1);
    }

    /// Every octave reading of a tempo is offered to the gate, or the
    /// correction above would never see the tracks it rescues.
    #[test]
    fn the_gate_accepts_any_octave() {
        assert!(in_window_any_octave(&FOCUS, 85.0, None));
        assert!(in_window_any_octave(&FOCUS, 170.0, None), "double");
        assert!(in_window_any_octave(&FOCUS, 42.5, None), "half");
        assert!(!in_window_any_octave(&FOCUS, 130.0, None));
        assert!(!in_window_any_octave(&FOCUS, 0.0, None), "no tempo at all");
    }

    /// No halving toward an open side: Sleep has no floor, and halving
    /// let every track up to 136 BPM in. Doubling toward its ceiling
    /// adds nothing either — whatever doubles under 68 is already under
    /// it — so a fast track stays out under every reading.
    #[test]
    fn no_octave_correction_toward_an_open_side() {
        assert!(in_window_any_octave(&SLEEP, 60.0, None));
        assert!(!in_window_any_octave(&SLEEP, 120.0, None), "not read as 60");
        assert_eq!(effective_bpm(&SLEEP, 120.0, None), (120.0, 1.0));
    }

    /// The loudness gate keeps out what is measured too loud, and only
    /// that: an unmeasured track is not evidence of a loud one.
    #[test]
    fn the_loudness_gate_excludes_only_measured_loud_tracks() {
        assert!(passes_loudness_gate(&SLEEP, Some(-20.0)));
        assert!(passes_loudness_gate(&SLEEP, Some(-12.0)), "inclusive");
        assert!(!passes_loudness_gate(&SLEEP, Some(-8.0)));
        assert!(passes_loudness_gate(&SLEEP, None));
        assert!(passes_loudness_gate(&FOCUS, Some(-3.0)), "no gate set");
    }

    /// A reading the estimator trusts is taken as it is: no octave
    /// reading rescues it into a window it does not fit, and the gate
    /// the queries ask says the same.
    #[test]
    fn a_trusted_tempo_is_never_reinterpreted() {
        assert_eq!(effective_bpm(&FOCUS, 170.0, Some(0.8)), (170.0, 1.0));
        assert!(!in_window_any_octave(&FOCUS, 170.0, Some(0.8)));
        // Below the threshold the octave readings stay open.
        assert_eq!(
            effective_bpm(&FOCUS, 170.0, Some(0.1)),
            (85.0, OCTAVE_PENALTY)
        );
        assert!(in_window_any_octave(&FOCUS, 170.0, Some(0.1)));
    }

    /// A doubtful tempo is weaker evidence than a sure one, in both
    /// directions: at the centre it scores lower, and nothing scores it
    /// as a miss.
    #[test]
    fn a_doubtful_tempo_counts_for_less() {
        let mut sure = candidate(1, 85.0, Some(-16.0));
        sure.bpm_confidence = Some(0.9);
        let mut doubtful = candidate(2, 85.0, Some(-16.0));
        doubtful.bpm_confidence = Some(0.1);
        assert!(fit_score(&FOCUS, &sure) > fit_score(&FOCUS, &doubtful));
    }

    /// Energy gates like loudness does, and an unmeasured one passes.
    #[test]
    fn the_energy_gates_keep_out_what_they_name() {
        let sleep = MoodProfile {
            energy_ceiling: Some(0.4),
            ..FOCUS
        };
        assert!(passes_energy_gate(&sleep, Some(0.2)));
        assert!(!passes_energy_gate(&sleep, Some(0.8)));
        assert!(passes_energy_gate(&sleep, None));
        let workout = MoodProfile {
            energy_floor: Some(0.4),
            ..FOCUS
        };
        assert!(!passes_energy_gate(&workout, Some(0.2)));
        assert!(passes_energy_gate(&workout, Some(0.7)));
    }

    /// Two tracks at the same tempo and level: the calmer one fits a
    /// calm mood better. Loudness could not tell them apart once a
    /// library is normalised; this is what energy is for.
    #[test]
    fn energy_separates_tracks_the_level_cannot() {
        let calm_mood = MoodProfile {
            energy_max: Some(0.4),
            ..FOCUS
        };
        let mut calm = candidate(1, 85.0, Some(-14.0));
        calm.energy = Some(0.2);
        let mut intense = candidate(2, 85.0, Some(-14.0));
        intense.energy = Some(0.9);
        let mut unknown = candidate(3, 85.0, Some(-14.0));
        unknown.energy = None;
        let (c, i, u) = (
            fit_score(&calm_mood, &calm),
            fit_score(&calm_mood, &intense),
            fit_score(&calm_mood, &unknown),
        );
        assert!(c > u && u > i, "calm {c} unknown {u} intense {i}");
    }
}
