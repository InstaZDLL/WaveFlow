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

/// Which of the two measurements a file can carry should be applied.
///
/// Track gain levels every track against every other one, which is
/// what you want when a song comes up shuffled between two unrelated
/// things. Album gain applies **one** gain across a whole record,
/// preserving the level relationships the mastering engineer put
/// inside it — the hushed intro stays quieter than the single — at
/// the cost of two different records no longer matching each other.
///
/// Neither is right in general, because the right answer depends on
/// *why* the track is playing. That is what [`GainMode::Auto`] is
/// for, and it is the default: the context is already known where the
/// gain is applied, so there is nothing for the listener to manage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GainMode {
    /// Always the track's own gain.
    Track,
    /// Always the album gain, falling back to the track's own on files
    /// that carry none.
    Album,
    /// Album gain while a record is being played through, track gain
    /// otherwise.
    #[default]
    Auto,
}

impl GainMode {
    /// Parse the persisted form. Anything unrecognised — including a
    /// row written by a future build — reads as the default rather
    /// than as an error: a setting nobody can spell is still a setting
    /// the user has to listen to.
    pub fn from_setting(value: &str) -> Self {
        match value {
            "track" => Self::Track,
            "album" => Self::Album,
            _ => Self::Auto,
        }
    }

    /// The persisted form. Stable — it is written into
    /// `profile_setting` and read back by older builds.
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Auto => "auto",
        }
    }

    /// The form the decoder thread reads, out of a plain integer
    /// atomic. Private to this pairing — nothing but
    /// [`SharedPlayback`](crate::audio::state::SharedPlayback) should
    /// care what number a mode is.
    pub fn as_bits(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Track => 1,
            Self::Album => 2,
        }
    }

    /// Inverse of [`Self::as_bits`], total on purpose: an unknown
    /// number means the atomic was written by code this build does not
    /// have, and the default is a better answer than a panic on the
    /// audio path.
    pub fn from_bits(bits: u8) -> Self {
        match bits {
            1 => Self::Track,
            2 => Self::Album,
            _ => Self::Auto,
        }
    }
}

/// Whether the track currently decoding is being listened to *as part
/// of its record*, which is the whole input [`GainMode::Auto`] keys
/// off.
///
/// Deliberately not a bare `bool` at the call sites: `effective_linear(
/// gain, settings, true)` says nothing about what is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listening {
    /// A record playing through in its own order — the queue was built
    /// from an album, or shuffle is grouping by album so whole records
    /// play in sequence.
    ToAnAlbum,
    /// One track among unrelated ones.
    ToATrack,
}

/// What is known about one track's loudness, whichever source it came
/// from. Both fields are independent: plenty of files carry a gain
/// with no peak.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TrackGain {
    /// Gain in dB on the ReplayGain 2.0 (−18 LUFS) scale.
    pub gain_db: Option<f64>,
    /// Linear sample peak. Above 1.0 on clipped masters.
    pub peak: Option<f64>,
    /// Set when `peak` came from an analysis row written before we
    /// started recording which pass produced it (`analysis_version IS
    /// NULL`). Such a peak was taken over a mono downmix, which
    /// **under-reports** an out-of-phase mix — the samples sit at full
    /// scale while the sum is near silent. Clipping prevention treats
    /// it as a lower bound rather than a measurement: see
    /// [`effective_gain_db`].
    ///
    /// Meaningless on its own — always read together with `peak`.
    pub peak_unverified: bool,
    /// Gain in dB for the **whole record** this track belongs to, on
    /// the same scale as [`Self::gain_db`].
    ///
    /// Tag-only, and that is not an oversight: our analysis pass runs
    /// per track and has no notion of an album, so there is nothing of
    /// our own to fall back on. A file gets album gain when a tagger
    /// wrote `REPLAYGAIN_ALBUM_GAIN` into it, and otherwise does not.
    pub album_gain_db: Option<f64>,
    /// Linear sample peak across the whole record — the loudest sample
    /// of its loudest track, so never below [`Self::peak`].
    ///
    /// Tag-only for the same reason, and never unverified: the doubt
    /// [`Self::peak_unverified`] carries belongs to a measurement we
    /// made, and we never made this one.
    pub album_peak: Option<f64>,
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
            // The doubt travels with the peak it belongs to, so a file
            // carrying its own `REPLAYGAIN_TRACK_PEAK` clears it: that
            // number came from a tagger, not from our superseded pass,
            // and it wins here anyway.
            peak_unverified: if tag.peak.is_some() {
                false
            } else {
                analysis.peak_unverified
            },
            // Straight from the tag, with no `or` to fall back on:
            // the analysis side of this merge never carries album
            // numbers, so an `analysis.album_gain_db` would only ever
            // be `None` pretending to be a decision.
            album_gain_db: tag.album_gain_db,
            album_peak: tag.album_peak,
        }
    }

    /// The gain and peak to actually apply, given the mode and how the
    /// track is being listened to.
    ///
    /// Two fallbacks, each for its own reason:
    ///
    /// - **No album gain → the track's own.** Album mode would
    ///   otherwise do nothing at all on an untagged file, and a
    ///   library that is half-tagged would jump every time it crossed
    ///   the line. Same reasoning as `fallback_db`.
    /// - **No album peak → the track's own peak.** This is the one
    ///   place the uniformity argument below is knowingly traded away:
    ///   an uneven cap is a cosmetic defect, and the cap only ever
    ///   binds where the alternative is audible clipping.
    fn resolved(self, mode: GainMode, listening: Listening) -> Self {
        let wants_album = match mode {
            GainMode::Track => false,
            GainMode::Album => true,
            GainMode::Auto => listening == Listening::ToAnAlbum,
        };
        let Some(album_gain_db) = self.album_gain_db.filter(|_| wants_album) else {
            return self;
        };
        Self {
            gain_db: Some(album_gain_db),
            // The **album** peak, which is the same number for every
            // track on the record. Capping each track by its own peak
            // instead would pull tracks down according to their own
            // loudest sample — re-introducing exactly the per-track
            // variation album mode exists to remove.
            peak: self.album_peak.or(self.peak),
            peak_unverified: if self.album_peak.is_some() {
                false
            } else {
                self.peak_unverified
            },
            ..self
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
    /// Which of a file's two gains to apply.
    pub mode: GainMode,
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
            mode: GainMode::Auto,
        }
    }
}

/// The gain to apply, in dB. Split out from [`effective_linear`] so
/// the decision is testable and can be logged in dB, which is the
/// unit everything about ReplayGain is expressed in.
pub fn effective_gain_db(track: TrackGain, settings: GainSettings, listening: Listening) -> f64 {
    if !settings.enabled {
        return 0.0;
    }

    // Which of the file's two measurements the mode asks for. Done
    // first so everything below reads one pair of numbers and cannot
    // accidentally mix an album gain with a track peak.
    let track = track.resolved(settings.mode, listening);

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
            let mut headroom_db = -20.0 * peak.log10();
            if track.peak_unverified {
                // The measurement is a lower bound, not a peak, so the
                // headroom derived from it is an over-estimate. Refuse
                // the boost it seems to allow, and keep any attenuation
                // it asks for: a downmix that still reports above full
                // scale means the real peak is higher again.
                headroom_db = headroom_db.min(0.0);
            }
            gain = gain.min(headroom_db);
        }
    }

    gain
}

/// The linear scalar for the decoder's multiply. Returns exactly
/// `1.0` when there is nothing to do, which lets the caller skip the
/// buffer walk entirely.
pub fn effective_linear(track: TrackGain, settings: GainSettings, listening: Listening) -> f32 {
    let db = effective_gain_db(track, settings, listening);
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

    /// Every test below this point describes a track heard on its own,
    /// which is what the whole file did before album mode existed.
    /// The album cases spell their context out instead.
    fn as_track_db(track: TrackGain, settings: GainSettings) -> f64 {
        effective_gain_db(track, settings, Listening::ToATrack)
    }

    fn as_track_linear(track: TrackGain, settings: GainSettings) -> f32 {
        effective_linear(track, settings, Listening::ToATrack)
    }

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
            peak_unverified: false,
            ..Default::default()
        };
        let settings = GainSettings {
            enabled: false,
            preamp_db: 6.0,
            fallback_db: -3.0,
            prevent_clipping: true,
            mode: GainMode::Auto,
        };
        assert!(approx(as_track_db(track, settings), 0.0));
        assert_eq!(as_track_linear(track, settings), 1.0);
    }

    /// The dB → linear conversion itself, in both directions:
    /// +6 dB is about twice the amplitude, −6 dB about half.
    #[test]
    fn six_decibels_is_a_doubling_of_amplitude() {
        let settings = GainSettings {
            prevent_clipping: false,
            ..on()
        };
        let up = as_track_linear(
            TrackGain {
                gain_db: Some(6.0),
                peak: None,
                peak_unverified: false,
                ..Default::default()
            },
            settings,
        );
        let down = as_track_linear(
            TrackGain {
                gain_db: Some(-6.0),
                peak: None,
                peak_unverified: false,
                ..Default::default()
            },
            settings,
        );
        assert!((up - 1.995).abs() < 0.01, "+6 dB gave {up}");
        assert!((down - 0.501).abs() < 0.01, "-6 dB gave {down}");
        assert_eq!(
            as_track_linear(
                TrackGain {
                    gain_db: Some(0.0),
                    peak: None,
                    peak_unverified: false,
                    ..Default::default()
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
            peak_unverified: false,
            ..Default::default()
        };
        let settings = GainSettings {
            preamp_db: 3.0,
            ..on()
        };
        assert!(approx(as_track_db(track, settings), -5.0));
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
            peak_unverified: false,
            ..Default::default()
        };
        let capped = as_track_db(track, on());
        assert!(
            (capped - 6.0206).abs() < 1e-3,
            "expected the 6 dB of headroom above a 0.5 peak, got {capped}"
        );

        // And the sample that peaked now lands at unity, not past it.
        let linear = as_track_linear(track, on());
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
            peak_unverified: false,
            ..Default::default()
        };
        let settings = GainSettings {
            prevent_clipping: false,
            ..on()
        };
        assert!(approx(as_track_db(track, settings), 9.0));
    }

    /// Attenuation is never held back by the peak — a quiet-peaking
    /// track that measured loud still gets turned down.
    #[test]
    fn clipping_prevention_never_raises_a_negative_gain() {
        let track = TrackGain {
            gain_db: Some(-6.0),
            peak: Some(0.1),
            peak_unverified: false,
            ..Default::default()
        };
        assert!(approx(as_track_db(track, on()), -6.0));
    }

    /// A master that already clips has negative headroom, so the
    /// limiter asks for attenuation even when the track's own gain
    /// was zero.
    #[test]
    fn a_master_that_already_clips_is_turned_down() {
        let track = TrackGain {
            gain_db: Some(0.0),
            peak: Some(1.25),
            peak_unverified: false,
            ..Default::default()
        };
        let gain = as_track_db(track, on());
        assert!(gain < -1.9 && gain > -2.0, "expected ~-1.94 dB, got {gain}");
    }

    #[test]
    fn a_track_nothing_knows_about_uses_the_fallback() {
        let settings = GainSettings {
            fallback_db: -4.0,
            ..on()
        };
        assert!(approx(as_track_db(TrackGain::default(), settings), -4.0));
    }

    #[test]
    fn the_file_tag_wins_over_our_own_analysis() {
        let tag = TrackGain {
            gain_db: Some(-7.0),
            peak: None,
            peak_unverified: false,
            ..Default::default()
        };
        let analysis = TrackGain {
            gain_db: Some(-3.0),
            peak: Some(0.9),
            peak_unverified: false,
            ..Default::default()
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
            peak_unverified: false,
            ..Default::default()
        };
        assert!(approx(as_track_db(loud, on()), MAX_TOTAL_GAIN_DB));

        let silent = TrackGain {
            gain_db: Some(-90.0),
            peak: None,
            peak_unverified: false,
            ..Default::default()
        };
        assert!(approx(as_track_db(silent, on()), MIN_TOTAL_GAIN_DB));
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
                peak_unverified: false,
                ..Default::default()
            };
            let headroom_db = -20.0 * peak.log10();
            let gain = as_track_db(track, on());
            assert!(
                gain <= headroom_db + 1e-9,
                "peak {peak} leaves {headroom_db} dB of headroom but the gain came out {gain}"
            );
            // And the loudest sample really does stay at or under
            // full scale.
            let scaled = f64::from(as_track_linear(track, on())) * peak;
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
                peak_unverified: false,
                ..Default::default()
            };
            assert!(
                approx(as_track_db(track, settings), -5.0),
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
                peak_unverified: false,
                ..Default::default()
            };
            assert!(
                approx(as_track_db(track, on()), 3.0),
                "peak {peak} should have been ignored"
            );
            assert!(as_track_linear(track, on()).is_finite());
        }
    }

    /// The bug this flag exists for. A pre-#545 row measured its peak
    /// over a mono downmix, so a quiet-summing mix reports far below
    /// the level its samples actually reach — and the headroom derived
    /// from it invites a boost that clips.
    #[test]
    fn an_unverified_peak_never_licenses_a_boost() {
        let settings = GainSettings {
            preamp_db: 6.0,
            ..on()
        };
        // A downmix peak of 0.1 claims 20 dB of headroom.
        let stale = TrackGain {
            gain_db: Some(3.0),
            peak: Some(0.1),
            peak_unverified: true,
            ..Default::default()
        };
        assert!(
            approx(as_track_db(stale, settings), 0.0),
            "an unverified peak must cap at unity, not at its own headroom"
        );
        // The same numbers from the current pass are trusted, so the
        // +9 dB fits inside the 20 dB the peak leaves.
        let fresh = TrackGain {
            peak_unverified: false,
            ..stale
        };
        assert!(approx(as_track_db(fresh, settings), 9.0));
    }

    /// Refusing the boost must not also refuse the attenuation. A
    /// downmix under-reports, so a stale row that *still* reads above
    /// full scale describes a master that clips even harder than it
    /// says — that request is honoured in full.
    #[test]
    fn an_unverified_peak_still_asks_for_its_attenuation() {
        let track = TrackGain {
            gain_db: Some(0.0),
            peak: Some(2.0),
            peak_unverified: true,
            ..Default::default()
        };
        // -20*log10(2) = -6.02 dB, unchanged by the unity cap.
        assert!(approx(as_track_db(track, on()), -20.0 * 2f64.log10()));
    }

    /// Turning clipping prevention off turns off the restriction with
    /// it: the flag is an input to the limiter, not a separate one.
    #[test]
    fn an_unverified_peak_is_moot_without_clipping_prevention() {
        let track = TrackGain {
            gain_db: Some(5.0),
            peak: Some(0.1),
            peak_unverified: true,
            ..Default::default()
        };
        let settings = GainSettings {
            prevent_clipping: false,
            ..on()
        };
        assert!(approx(as_track_db(track, settings), 5.0));
    }

    /// The doubt belongs to the peak, so a file carrying its own
    /// `REPLAYGAIN_TRACK_PEAK` clears it — that number never came from
    /// our superseded pass, and it is the one being used.
    #[test]
    fn a_tag_peak_clears_the_doubt_the_analysis_carried() {
        let tag = TrackGain {
            gain_db: None,
            peak: Some(0.5),
            peak_unverified: false,
            ..Default::default()
        };
        let analysis = TrackGain {
            gain_db: Some(4.0),
            peak: Some(0.1),
            peak_unverified: true,
            ..Default::default()
        };
        let merged = TrackGain::prefer_tag(tag, analysis);
        assert_eq!(merged.peak, Some(0.5));
        assert!(!merged.peak_unverified);

        // With no tag peak to replace it, the doubt survives the merge.
        let no_tag_peak = TrackGain { peak: None, ..tag };
        assert!(TrackGain::prefer_tag(no_tag_peak, analysis).peak_unverified);
    }

    // ------------------------------------------------------------------
    // Album mode (#587)
    // ------------------------------------------------------------------

    /// A record: quieter than most, so its album gain asks for a boost,
    /// and one track on it peaks much higher than this one does.
    fn record() -> TrackGain {
        TrackGain {
            gain_db: Some(-3.0),
            peak: Some(0.4),
            peak_unverified: false,
            album_gain_db: Some(-7.0),
            album_peak: Some(0.98),
        }
    }

    fn with_mode(mode: GainMode) -> GainSettings {
        GainSettings { mode, ..on() }
    }

    /// The point of the feature: the same file gets a different gain
    /// depending on which measurement the mode asks for.
    #[test]
    fn the_mode_chooses_which_of_the_two_gains_applies() {
        let track = TrackGain {
            peak: None,
            album_peak: None,
            ..record()
        };
        assert!(approx(
            effective_gain_db(track, with_mode(GainMode::Track), Listening::ToAnAlbum),
            -3.0,
        ));
        assert!(approx(
            effective_gain_db(track, with_mode(GainMode::Album), Listening::ToATrack),
            -7.0,
        ));
    }

    /// `Auto` reads the context instead of asking the listener: album
    /// gain while the record plays through, track gain for the same
    /// file heard between two unrelated things.
    #[test]
    fn automatic_follows_the_listening_context() {
        let track = TrackGain {
            peak: None,
            album_peak: None,
            ..record()
        };
        let auto = with_mode(GainMode::Auto);
        assert!(approx(
            effective_gain_db(track, auto, Listening::ToAnAlbum),
            -7.0
        ));
        assert!(approx(
            effective_gain_db(track, auto, Listening::ToATrack),
            -3.0
        ));
    }

    /// The trap the issue names. Clipping prevention must cap by the
    /// **album** peak in album mode: the album peak is the same number
    /// for every track on the record, so the cap is uniform. Capping
    /// each track by its own peak would pull tracks down according to
    /// their own loudest sample — which is exactly the per-track
    /// variation album mode exists to remove.
    #[test]
    fn album_mode_caps_by_the_album_peak_not_the_track_peak() {
        let loud_preamp = GainSettings {
            preamp_db: 12.0,
            ..with_mode(GainMode::Album)
        };
        let gain = effective_gain_db(record(), loud_preamp, Listening::ToAnAlbum);

        // 0.98 leaves 0.175 dB of headroom; the track's own 0.4 peak
        // would have licensed almost 8 dB.
        let album_headroom = -20.0 * 0.98_f64.log10();
        assert!(
            approx(gain, album_headroom),
            "expected the album's {album_headroom} dB of headroom, got {gain}"
        );

        // And the whole record shares that cap, so two tracks with very
        // different peaks of their own still come out level.
        let quiet_track = TrackGain {
            peak: Some(0.05),
            ..record()
        };
        assert!(approx(
            effective_gain_db(quiet_track, loud_preamp, Listening::ToAnAlbum),
            gain,
        ));
    }

    /// Album gain exists only where a tagger wrote it, because our own
    /// analysis pass measures one track at a time. A file without it
    /// falls back to its track gain rather than to nothing — otherwise
    /// album mode would do nothing at all on a half-tagged library,
    /// which is the jump `fallback_db` exists to prevent.
    #[test]
    fn a_file_with_no_album_gain_falls_back_to_its_own() {
        let untagged = TrackGain {
            album_gain_db: None,
            album_peak: None,
            ..record()
        };
        assert!(approx(
            effective_gain_db(untagged, with_mode(GainMode::Album), Listening::ToAnAlbum),
            -3.0,
        ));
    }

    /// The peak falls back with it, and this one is a deliberate trade
    /// against the uniformity argument above: an uneven cap is
    /// cosmetic, and the cap only ever binds where the alternative is
    /// audible clipping.
    #[test]
    fn an_album_gain_without_an_album_peak_still_gets_clipping_prevention() {
        let no_album_peak = TrackGain {
            album_peak: None,
            peak: Some(0.5),
            ..record()
        };
        let settings = GainSettings {
            preamp_db: 12.0,
            ..with_mode(GainMode::Album)
        };
        let gain = effective_gain_db(no_album_peak, settings, Listening::ToAnAlbum);
        assert!(
            approx(gain, -20.0 * 0.5_f64.log10()),
            "expected the track peak to stand in, got {gain}"
        );
    }

    /// The doubt on a pre-#545 analysis peak belongs to a measurement
    /// we made, and we never measured an album — so an album peak read
    /// from a tag clears it, exactly as a track tag peak does.
    #[test]
    fn an_album_peak_is_never_unverified() {
        let stale = TrackGain {
            peak: Some(0.1),
            peak_unverified: true,
            album_peak: Some(0.5),
            ..record()
        };
        let settings = GainSettings {
            preamp_db: 12.0,
            ..with_mode(GainMode::Album)
        };
        let gain = effective_gain_db(stale, settings, Listening::ToAnAlbum);
        assert!(
            approx(gain, -20.0 * 0.5_f64.log10()),
            "the tagged album peak should be trusted in full, got {gain}"
        );

        // With no album peak to replace it, the doubt survives and the
        // boost is refused.
        let no_album_peak = TrackGain {
            album_peak: None,
            ..stale
        };
        assert!(approx(
            effective_gain_db(no_album_peak, settings, Listening::ToAnAlbum),
            0.0,
        ));
    }

    /// Album numbers are tag-only, so the merge must not invent an
    /// analysis side for them.
    #[test]
    fn the_album_pair_comes_from_the_tag_alone() {
        let tag = TrackGain {
            gain_db: None,
            peak: None,
            peak_unverified: false,
            album_gain_db: Some(-5.0),
            album_peak: Some(0.9),
        };
        let analysis = TrackGain {
            gain_db: Some(-2.0),
            peak: Some(0.3),
            peak_unverified: false,
            album_gain_db: None,
            album_peak: None,
        };
        let merged = TrackGain::prefer_tag(tag, analysis);
        assert_eq!(merged.album_gain_db, Some(-5.0));
        assert_eq!(merged.album_peak, Some(0.9));
        // …and the track pair still comes from the analysis.
        assert_eq!(merged.gain_db, Some(-2.0));
        assert_eq!(merged.peak, Some(0.3));
    }

    /// The mode is persisted as a string other builds read back, so the
    /// round trip is part of the contract. An unknown value is the
    /// default, not an error: a row written by a future build must not
    /// leave the listener without a setting.
    #[test]
    fn the_persisted_mode_round_trips_and_tolerates_nonsense() {
        for mode in [GainMode::Track, GainMode::Album, GainMode::Auto] {
            assert_eq!(GainMode::from_setting(mode.as_setting()), mode);
            assert_eq!(GainMode::from_bits(mode.as_bits()), mode);
        }
        for junk in ["", "ALBUM", "per-album", "3"] {
            assert_eq!(GainMode::from_setting(junk), GainMode::Auto);
        }
        for junk in [3u8, 7, 255] {
            assert_eq!(GainMode::from_bits(junk), GainMode::Auto);
        }
    }

    /// Off is off, whatever the mode says.
    #[test]
    fn the_mode_does_nothing_while_replaygain_is_disabled() {
        let settings = GainSettings {
            enabled: false,
            mode: GainMode::Album,
            ..Default::default()
        };
        assert!(approx(
            effective_gain_db(record(), settings, Listening::ToAnAlbum),
            0.0
        ));
    }
}
