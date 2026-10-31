use super::*;
use crate::analysis::beats;

/// Beats every `60 / bpm` seconds from `first` to `last`, through the
/// stored encoding so the millisecond rounding of a real row is in.
fn grid(bpm: f64, first: f64, last: f64) -> Vec<f64> {
    let period = 60.0 / bpm;
    let n = ((last - first) / period).floor() as usize + 1;
    let raw: Vec<f64> = (0..n).map(|k| first + k as f64 * period).collect();
    beats::decode(&beats::encode(&raw).unwrap()).unwrap()
}

/// A small deterministic scatter, -max..max seconds, standing in for the
/// tracker placing each beat a little early or late.
fn jitter(beats: &mut [f64], max: f64) {
    let mut x: u32 = 0x9e37_79b9;
    for b in beats.iter_mut() {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let unit = f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0;
        *b += unit * max;
    }
}

fn track(beats: &[f64], duration_s: f64) -> TrackBeats<'_> {
    TrackBeats {
        beats,
        duration_ms: (duration_s * 1000.0) as u64,
    }
}

/// Where the incoming beats land against the outgoing ones under a plan:
/// for every outgoing beat inside the fade, the distance to the incoming
/// beat it was lined up with, in milliseconds. Measured on the beat lists
/// themselves, not on the fitted grids the plan was made from.
fn measured_drift(out: &[f64], inc: &[f64], plan: &Plan) -> f64 {
    let offset = plan.fade_start_us as f64 / 1e6;
    let end = plan.fade_end_us as f64 / 1e6 - 0.001;
    let first_in = inc[0] + offset;
    out.iter()
        .filter(|&&b| b >= first_in - 0.05 && b <= end)
        .filter_map(|&b| {
            inc.iter()
                .map(|&i| (i + offset - b).abs())
                .min_by(f64::total_cmp)
        })
        // Lines between two beats of a half-tempo track have no partner.
        .filter(|d| *d < 0.2)
        .fold(0.0, f64::max)
        * 1000.0
}

#[test]
fn a_steady_grid_is_fitted_exactly() {
    let b = grid(120.0, 0.25, 30.0);
    let g = Grid::fit(&b).unwrap();
    assert!((g.period - 0.5).abs() < 1e-4, "{g:?}");
    assert!((g.origin - 0.25).abs() < 1e-3, "{g:?}");
    assert!(g.scatter < 1e-3);
}

#[test]
fn same_tempo_mixes_over_the_longest_fade_the_setting_holds() {
    let out = grid(120.0, 0.5, 200.0);
    let inc = grid(120.0, 0.3, 180.0);
    // 8 s at 120 BPM: 16 beats, and the 0.3 s intro takes the 16th's
    // room, so 8 beats (4 s) plus the intro.
    let p = plan(track(&out, 204.0), track(&inc, 180.0), 8_000).unwrap();
    assert_eq!(p.beats, 8, "{p:?}");
    assert_eq!(p.fold, 1.0);
    // The fade ends one beat after the last beat.
    assert!((p.fade_end_us as f64 / 1e6 - 200.5).abs() < 0.01, "{p:?}");
    // The incoming track's first beat lands on an outgoing beat: its
    // first sample plays 0.3 s before one.
    let first_in = p.fade_start_us as f64 / 1e6 + 0.3;
    let nearest = out
        .iter()
        .map(|b| (b - first_in).abs())
        .fold(f64::MAX, f64::min);
    assert!(
        nearest < 0.002,
        "first beat {nearest} s off an outgoing beat"
    );
    assert!(p.max_drift_ms < 2.0, "{p:?}");
    assert!(measured_drift(&out, &inc, &p) < 2.0);
    // Within the setting, intro included.
    assert!(p.fade_end_us - p.fade_start_us <= 8_000_000);
}

#[test]
fn a_close_tempo_shortens_the_fade_until_its_ends_hold() {
    let out = grid(120.0, 0.5, 200.0);
    // 1.5 % slower: 16 beats would drift 8 × 7.5 ms = 60 ms at the ends,
    // 8 beats 30 ms, 4 beats 15 ms.
    let inc = grid(120.0 / 1.015, 0.0, 180.0);
    let p = plan(track(&out, 201.0), track(&inc, 180.0), 12_000).unwrap();
    assert!(p.beats <= 8, "{p:?}");
    assert!(p.max_drift_ms <= MAX_DRIFT_MS, "{p:?}");
    let measured = measured_drift(&out, &inc, &p);
    assert!(measured <= MAX_DRIFT_MS + 1.0, "measured {measured} ms");
}

/// The threshold, measured: on either side of the tempo gap a 4-beat
/// fade can hold at 120 BPM (30 ms / 1 s = 3 %).
#[test]
fn the_tempo_threshold_is_where_a_bar_drifts_past_the_limit() {
    let out = grid(120.0, 0.5, 200.0);
    for (gap, expect_plan) in [(0.025, true), (0.029, true), (0.032, false), (0.05, false)] {
        let inc = grid(120.0 / (1.0 + gap), 0.0, 180.0);
        let r = plan(track(&out, 201.0), track(&inc, 180.0), 12_000);
        match (expect_plan, r) {
            (true, Ok(p)) => {
                assert_eq!(p.beats, 4, "gap {gap}: {p:?}");
                let measured = measured_drift(&out, &inc, &p);
                assert!(measured <= MAX_DRIFT_MS + 1.0, "gap {gap}: {measured} ms");
            }
            (false, Err(Fallback::TempoMismatch { ratio })) => {
                assert!((ratio - (1.0 + gap)).abs() < 0.002, "{ratio}");
            }
            (_, other) => panic!("gap {gap}: {other:?}"),
        }
    }
}

#[test]
fn double_and_half_tempo_count_as_the_same() {
    let out = grid(128.0, 0.5, 200.0);
    let half = grid(64.0, 0.4, 180.0);
    let p = plan(track(&out, 201.0), track(&half, 180.0), 8_000).unwrap();
    assert_eq!(p.fold, 2.0, "{p:?}");
    assert!(measured_drift(&out, &half, &p) < 2.0);

    let double = grid(256.0, 0.1, 180.0);
    let p = plan(track(&out, 201.0), track(&double, 180.0), 8_000).unwrap();
    assert_eq!(p.fold, 0.5, "{p:?}");
    assert!(measured_drift(&out, &double, &p) < 2.0);
}

#[test]
fn three_against_two_is_not_a_fold() {
    let out = grid(120.0, 0.5, 200.0);
    let inc = grid(180.0, 0.0, 180.0);
    assert!(matches!(
        plan(track(&out, 201.0), track(&inc, 180.0), 8_000),
        Err(Fallback::TempoMismatch { .. })
    ));
}

/// The tracker's usual scatter does not move the alignment: the beats
/// still land within a few milliseconds of each other on average, and
/// the plan stays.
#[test]
fn tracker_jitter_is_averaged_out_by_the_fit() {
    let mut out = grid(124.0, 0.5, 200.0);
    let mut inc = grid(124.0, 0.2, 180.0);
    jitter(&mut out, 0.015);
    jitter(&mut inc, 0.015);
    let p = plan(track(&out, 201.0), track(&inc, 180.0), 8_000).unwrap();
    assert!(p.max_drift_ms < 5.0, "{p:?}");
}

#[test]
fn a_grid_that_slips_half_a_beat_is_not_trusted() {
    let out = grid(120.0, 0.5, 200.0);
    let mut inc = grid(120.0, 0.0, 180.0);
    // Half the fitted run off the beat by half a period.
    for b in inc[8..24].iter_mut() {
        *b += 0.25;
    }
    assert_eq!(
        plan(track(&out, 201.0), track(&inc, 180.0), 8_000),
        Err(Fallback::UnsteadyGrid)
    );
}

#[test]
fn no_grid_or_too_few_beats_falls_back() {
    let out = grid(120.0, 0.5, 200.0);
    let short = grid(120.0, 0.0, 2.0);
    assert_eq!(
        plan(track(&out, 201.0), track(&[], 180.0), 8_000),
        Err(Fallback::NoGrid)
    );
    assert_eq!(
        plan(track(&out, 201.0), track(&short, 180.0), 8_000),
        Err(Fallback::NoGrid)
    );
    assert_eq!(
        plan(track(&short, 201.0), track(&out, 180.0), 8_000),
        Err(Fallback::NoGrid)
    );
}

/// The beats stop where the music thins out: the fade ends there, not at
/// the end of the file, unless that would cut more than half a minute.
#[test]
fn the_exit_is_after_the_last_beat_not_at_the_end_of_the_file() {
    let out = grid(120.0, 0.5, 180.0);
    let inc = grid(120.0, 0.0, 180.0);
    // A 12 s fade-out after the beats stop.
    let p = plan(track(&out, 192.0), track(&inc, 180.0), 8_000).unwrap();
    assert!((p.fade_end_us as f64 / 1e6 - 180.5).abs() < 0.01, "{p:?}");
    // A 40 s coda after them is not cut.
    assert_eq!(
        plan(track(&out, 220.0), track(&inc, 180.0), 8_000),
        Err(Fallback::LongOutro)
    );
    // And a file that ends on its last beat ends the fade with it.
    let p = plan(track(&out, 180.2), track(&inc, 180.0), 8_000).unwrap();
    assert_eq!(p.fade_end_us, 180_200_000);
}

#[test]
fn an_intro_longer_than_the_fade_falls_back() {
    let out = grid(120.0, 0.5, 200.0);
    let inc = grid(120.0, 20.0, 180.0);
    assert_eq!(
        plan(track(&out, 201.0), track(&inc, 180.0), 8_000),
        Err(Fallback::LongIntro)
    );
}

#[test]
fn a_short_setting_gets_fewer_beats_and_none_is_no_room() {
    let out = grid(120.0, 0.5, 200.0);
    let inc = grid(120.0, 0.0, 180.0);
    let p = plan(track(&out, 201.0), track(&inc, 180.0), 1_000).unwrap();
    assert_eq!(p.beats, 2, "{p:?}");
    assert_eq!(
        plan(track(&out, 201.0), track(&inc, 180.0), 400),
        Err(Fallback::NoRoom)
    );
}

#[test]
fn the_earliest_start_leaves_room_for_the_longest_fade() {
    let out = grid(120.0, 0.5, 200.0);
    let at = earliest_fade_start_ms(track(&out, 201.0), 8_000).unwrap();
    assert_eq!(at, 192_500);
    let inc = grid(120.0, 0.3, 180.0);
    let p = plan(track(&out, 201.0), track(&inc, 180.0), 8_000).unwrap();
    assert!(p.fade_start_us / 1000 >= at);
    assert_eq!(earliest_fade_start_ms(track(&[], 201.0), 8_000), None);
}
