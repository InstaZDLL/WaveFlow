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
