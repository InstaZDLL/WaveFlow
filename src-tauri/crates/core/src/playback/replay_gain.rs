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
    /// atomic. Private to this pairing — nothing but the player's
    /// shared playback state should care what number a mode is.
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
        // A non-finite album gain counts as absent rather than as a
        // choice. Selecting it would hand `effective_gain_db` a pair
        // it has to throw away — and the fallback it lands on is
        // `fallback_db`, not this file's perfectly good track gain.
        let Some(album_gain_db) = self
            .album_gain_db
            .filter(|gain| wants_album && gain.is_finite())
        else {
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
mod tests;
