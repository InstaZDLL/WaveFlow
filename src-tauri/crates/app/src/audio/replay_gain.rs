//! Turning what we know about a track's loudness into the scalar the
//! decoder multiplies its samples by.
//!
//! The knowledge comes from two places — the file's own
//! `REPLAYGAIN_*` / `R128_*` tags, read by the scanner, and our own
//! BS.1770 analysis pass — both on the same −18 LUFS scale, so
//! [`TrackGain`] doesn't care which one it got. What it adds on top is
//! the three things a ReplayGain implementation is expected to have
//! and this one didn't:
//!
//! - a **pre-amp**, because −18 LUFS is quieter than most listeners
//!   set their system volume for, so a correctly-normalised library
//!   sounds like it lost volume;
//! - a **fallback** gain for tracks nothing knows about, so a library
//!   that is half-tagged doesn't jump every time it crosses the line;
//! - **clipping prevention**, which is the one that actually protects
//!   the sound. A +6 dB boost on a track that already peaks at 0.9
//!   pushes samples to 1.8, and the clamp at the end of the decoder
//!   chain flattens every one of them into distortion. Knowing the
//!   peak, we can just not ask for that much gain.
//!
//! All of it is pure arithmetic on the decoder thread, evaluated once
//! per decoded buffer rather than baked in at load time, so moving the
//! pre-amp slider is audible immediately instead of at the next track.

/// Hard ceiling on the gain we will ever apply, in dB. Clipping
/// prevention normally keeps positive gains well under this; the
/// ceiling is what stands between a corrupt tag that got past the
/// parser's own bounds and a pair of speakers.
const MAX_TOTAL_GAIN_DB: f64 = 12.0;
/// Floor, in dB. Nothing musical needs more attenuation than this,
/// and a value below it is a broken tag rather than a quiet track.
const MIN_TOTAL_GAIN_DB: f64 = -30.0;

/// What is known about one track's loudness, whichever source it came
/// from. Both fields are independent: plenty of files carry a gain
/// with no peak.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TrackGain {
    /// Gain in dB on the ReplayGain 2.0 (−18 LUFS) scale.
    pub gain_db: Option<f64>,
    /// Linear sample peak. Above 1.0 on clipped masters.
    pub peak: Option<f64>,
}

impl TrackGain {
    /// Prefer what the file says about itself over what we measured.
    ///
    /// A tag written by a dedicated scanner has usually seen the whole
    /// album and is what other players will use on the same file, so
    /// following it keeps us consistent with the rest of the user's
    /// tools. Each field falls back independently — a tagger that
    /// wrote a gain but no peak still gets clipping prevention from
    /// our analysis.
    pub fn prefer_tag(tag: TrackGain, analysis: TrackGain) -> Self {
        Self {
            gain_db: tag.gain_db.or(analysis.gain_db),
            peak: tag.peak.or(analysis.peak),
        }
    }
}

/// The user-facing knobs, read fresh from `SharedPlayback` on every
/// decoded buffer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainSettings {
    /// Master switch. When off, nothing here applies.
    pub enabled: bool,
    /// Added to every track's gain, in dB.
    pub preamp_db: f64,
    /// Used in place of a gain for tracks that have none.
    pub fallback_db: f64,
    /// Hold the gain back so the track's peak stays under full scale.
    pub prevent_clipping: bool,
}

impl Default for GainSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            fallback_db: 0.0,
            // On by default: a user who turns ReplayGain on is asking
            // for even loudness, not for distortion on the loud ones.
            prevent_clipping: true,
        }
    }
}

/// The gain to apply, in dB. Split out from [`effective_linear`] so
/// the decision is testable and can be logged in dB, which is the
/// unit everything about ReplayGain is expressed in.
pub fn effective_gain_db(track: TrackGain, settings: GainSettings) -> f64 {
    if !settings.enabled {
        return 0.0;
    }

    // A track with no gain from either source falls back rather than
    // playing at unity: a library where half the tracks are normalised
    // and half are not is worse than one that is uniformly off. A
    // non-finite value stored by an older build is treated the same
    // way — `clamp` propagates NaN rather than bounding it, so it has
    // to be caught before the arithmetic, not after.
    let base = track
        .gain_db
        .filter(|db| db.is_finite())
        .unwrap_or(settings.fallback_db);
    // Bound first, cap second. The other order lets the floor undo the
    // limiter: a peak needing more than `MIN_TOTAL_GAIN_DB` of
    // attenuation would be capped correctly and then raised back above
    // its own headroom, which is exactly the clipping this is meant to
    // prevent.
    let mut gain = (base + settings.preamp_db).clamp(MIN_TOTAL_GAIN_DB, MAX_TOTAL_GAIN_DB);

    if settings.prevent_clipping {
        if let Some(peak) = track.peak.filter(|p| p.is_finite() && *p > 0.0) {
            // The gain that lands the loudest sample exactly at full
            // scale. Negative for a track that already clips, which
            // correctly asks for attenuation. `min` only ever lowers,
            // so an absurdly small peak can't turn into a boost.
            let headroom_db = -20.0 * peak.log10();
            gain = gain.min(headroom_db);
        }
    }

    gain
}

/// The linear scalar for the decoder's multiply. Returns exactly
/// `1.0` when there is nothing to do, which lets the caller skip the
/// buffer walk entirely.
pub fn effective_linear(track: TrackGain, settings: GainSettings) -> f32 {
    let db = effective_gain_db(track, settings);
    if db == 0.0 {
        return 1.0;
    }
    let linear = 10f64.powf(db / 20.0);
    if linear.is_finite() {
        linear as f32
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> GainSettings {
        GainSettings {
            enabled: true,
            ..Default::default()
        }
    }

    fn approx(got: f64, want: f64) -> bool {
        (got - want).abs() < 1e-9
    }

    #[test]
    fn a_disabled_setting_applies_nothing_at_all() {
        let track = TrackGain {
            gain_db: Some(-9.0),
            peak: Some(0.5),
        };
        let settings = GainSettings {
            enabled: false,
            preamp_db: 6.0,
            fallback_db: -3.0,
            prevent_clipping: true,
        };
        assert!(approx(effective_gain_db(track, settings), 0.0));
        assert_eq!(effective_linear(track, settings), 1.0);
    }

    /// The dB → linear conversion itself, in both directions:
    /// +6 dB is about twice the amplitude, −6 dB about half.
    #[test]
    fn six_decibels_is_a_doubling_of_amplitude() {
        let settings = GainSettings {
            prevent_clipping: false,
            ..on()
        };
        let up = effective_linear(
            TrackGain {
                gain_db: Some(6.0),
                peak: None,
            },
            settings,
        );
        let down = effective_linear(
            TrackGain {
                gain_db: Some(-6.0),
                peak: None,
            },
            settings,
        );
        assert!((up - 1.995).abs() < 0.01, "+6 dB gave {up}");
        assert!((down - 0.501).abs() < 0.01, "-6 dB gave {down}");
        assert_eq!(
            effective_linear(
                TrackGain {
                    gain_db: Some(0.0),
                    peak: None
                },
                settings
            ),
            1.0
        );
    }

    #[test]
    fn the_preamp_is_added_to_the_track_gain() {
        let track = TrackGain {
            gain_db: Some(-8.0),
            peak: None,
        };
        let settings = GainSettings {
            preamp_db: 3.0,
            ..on()
        };
        assert!(approx(effective_gain_db(track, settings), -5.0));
    }

    /// The point of the whole exercise: a boost that would push the
    /// loudest sample past full scale gets held back to exactly full
    /// scale instead of being clipped flat afterwards.
    #[test]
    fn clipping_prevention_caps_the_gain_at_the_available_headroom() {
        // A peak of 0.5 leaves exactly 6.02 dB of headroom.
        let track = TrackGain {
            gain_db: Some(12.0),
            peak: Some(0.5),
        };
        let capped = effective_gain_db(track, on());
        assert!(
            (capped - 6.0206).abs() < 1e-3,
            "expected the 6 dB of headroom above a 0.5 peak, got {capped}"
        );

        // And the sample that peaked now lands at unity, not past it.
        let linear = effective_linear(track, on());
        assert!(
            (f64::from(linear) * 0.5 - 1.0).abs() < 1e-3,
            "0.5 scaled by {linear} should reach full scale"
        );
    }

    #[test]
    fn clipping_prevention_can_be_turned_off() {
        let track = TrackGain {
            gain_db: Some(9.0),
            peak: Some(0.5),
        };
        let settings = GainSettings {
            prevent_clipping: false,
            ..on()
        };
        assert!(approx(effective_gain_db(track, settings), 9.0));
    }

    /// Attenuation is never held back by the peak — a quiet-peaking
    /// track that measured loud still gets turned down.
    #[test]
    fn clipping_prevention_never_raises_a_negative_gain() {
        let track = TrackGain {
            gain_db: Some(-6.0),
            peak: Some(0.1),
        };
        assert!(approx(effective_gain_db(track, on()), -6.0));
    }

    /// A master that already clips has negative headroom, so the
    /// limiter asks for attenuation even when the track's own gain
    /// was zero.
    #[test]
    fn a_master_that_already_clips_is_turned_down() {
        let track = TrackGain {
            gain_db: Some(0.0),
            peak: Some(1.25),
        };
        let gain = effective_gain_db(track, on());
        assert!(gain < -1.9 && gain > -2.0, "expected ~-1.94 dB, got {gain}");
    }

    #[test]
    fn a_track_nothing_knows_about_uses_the_fallback() {
        let settings = GainSettings {
            fallback_db: -4.0,
            ..on()
        };
        assert!(approx(
            effective_gain_db(TrackGain::default(), settings),
            -4.0
        ));
    }

    #[test]
    fn the_file_tag_wins_over_our_own_analysis() {
        let tag = TrackGain {
            gain_db: Some(-7.0),
            peak: None,
        };
        let analysis = TrackGain {
            gain_db: Some(-3.0),
            peak: Some(0.9),
        };
        let merged = TrackGain::prefer_tag(tag, analysis);
        // Gain from the tag, peak from the analysis — a tagger that
        // wrote no peak still gets clipping prevention.
        assert_eq!(merged.gain_db, Some(-7.0));
        assert_eq!(merged.peak, Some(0.9));
    }

    /// A tag that somehow got past the parser's own bounds, or a
    /// pre-amp cranked to the top on an already-boosted track, must
    /// not reach the mixer as a 40 dB multiplier.
    #[test]
    fn the_total_gain_is_bounded_in_both_directions() {
        let loud = TrackGain {
            gain_db: Some(40.0),
            peak: None,
        };
        assert!(approx(effective_gain_db(loud, on()), MAX_TOTAL_GAIN_DB));

        let silent = TrackGain {
            gain_db: Some(-90.0),
            peak: None,
        };
        assert!(approx(effective_gain_db(silent, on()), MIN_TOTAL_GAIN_DB));
    }

    /// The floor must not undo the limiter. A peak above 31.62 needs
    /// more than `MIN_TOTAL_GAIN_DB` of attenuation, and clamping
    /// after the cap used to raise the gain back above the track's own
    /// headroom — re-introducing the clipping this exists to prevent.
    #[test]
    fn the_floor_never_raises_a_gain_back_above_its_headroom() {
        for peak in [31.62_f64, 100.0, 1_000.0] {
            let track = TrackGain {
                gain_db: Some(0.0),
                peak: Some(peak),
            };
            let headroom_db = -20.0 * peak.log10();
            let gain = effective_gain_db(track, on());
            assert!(
                gain <= headroom_db + 1e-9,
                "peak {peak} leaves {headroom_db} dB of headroom but the gain came out {gain}"
            );
            // And the loudest sample really does stay at or under
            // full scale.
            let scaled = f64::from(effective_linear(track, on())) * peak;
            assert!(scaled <= 1.0 + 1e-6, "peak {peak} scaled to {scaled}");
        }
    }

    /// A gain stored as NaN by an older build must not poison the
    /// multiply: `f64::clamp` propagates NaN instead of bounding it,
    /// so it is filtered out before the arithmetic and the track
    /// falls back like an untagged one.
    #[test]
    fn a_nonfinite_gain_falls_back_instead_of_propagating() {
        let settings = GainSettings {
            fallback_db: -5.0,
            ..on()
        };
        for bogus in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let track = TrackGain {
                gain_db: Some(bogus),
                peak: None,
            };
            assert!(
                approx(effective_gain_db(track, settings), -5.0),
                "gain {bogus} should have fallen back"
            );
        }
    }

    /// A zero or negative peak would make the headroom infinite;
    /// it must be ignored rather than propagated into the multiply.
    #[test]
    fn a_nonsensical_peak_is_ignored_rather_than_trusted() {
        for peak in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let track = TrackGain {
                gain_db: Some(3.0),
                peak: Some(peak),
            };
            assert!(
                approx(effective_gain_db(track, on()), 3.0),
                "peak {peak} should have been ignored"
            );
            assert!(effective_linear(track, on()).is_finite());
        }
    }
}
