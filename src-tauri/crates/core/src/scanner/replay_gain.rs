//! ReplayGain values read off the file's own tags.
//!
//! Until now the only gain WaveFlow knew about was the one its own
//! analysis pass computed, which meant a library tagged by rsgain,
//! foobar2000, beets or MusicBrainz Picard arrived with all that work
//! invisible and every track had to be re-analysed to get it back.
//! These are the tags that were being ignored.
//!
//! ## Two scales
//!
//! `REPLAYGAIN_*` (ReplayGain 2.0) is referenced to **−18 LUFS** and
//! written as text: `"-7.89 dB"`, sometimes without the unit, with
//! either sign. `R128_*` (the Opus/Vorbis convention) is referenced to
//! **−23 LUFS** and written as a **Q7.8 fixed-point integer** — a
//! whole number of 1/256 LU. They differ by exactly the 5 LU between
//! the two reference levels, which is the conversion applied here so
//! everything downstream sees one scale: ours, −18 LUFS, the same one
//! [`crate::analysis`] measures against.
//!
//! ## What is deliberately not done here
//!
//! An Opus stream also carries an `output gain` field in its header,
//! which a decoder is required to apply before handing samples out.
//! A file with both a non-zero header gain and an `R128_TRACK_GAIN`
//! would be adjusted twice if we added them. Taggers overwhelmingly
//! leave the header at 0 and write the tag, so the tag is what we
//! read; a file that does the opposite gets no gain from us rather
//! than a wrong one.

use lofty::tag::{ItemKey, Tag};

/// Sanity bound, in dB, on a gain read from a tag. Real-world values
/// live within roughly ±20 dB; anything beyond this is a broken or
/// misparsed tag and gets dropped rather than handed to the mixer.
const MAX_PLAUSIBLE_GAIN_DB: f64 = 60.0;
/// Sample peaks above this are not "loud", they're a wrong unit — a
/// peak written in dB, or a scale mismatch. Intersample peaks on
/// clipped masters do legitimately exceed 1.0, hence the headroom.
const MAX_PLAUSIBLE_PEAK: f64 = 4.0;
/// Distance between the ReplayGain 2.0 reference (−18 LUFS) and the
/// EBU R128 one (−23 LUFS).
const R128_TO_REPLAY_GAIN_LU: f64 = 5.0;

/// What a file's tags claim about its own loudness. Every field is
/// independent: plenty of taggers write gains without peaks.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ReplayGainTags {
    /// Track gain in dB, already on the −18 LUFS scale.
    pub track_gain_db: Option<f64>,
    /// Linear track peak, 0..1 for material that doesn't clip.
    pub track_peak: Option<f64>,
    /// Album gain in dB, on the −18 LUFS scale.
    pub album_gain_db: Option<f64>,
    /// Linear album peak.
    pub album_peak: Option<f64>,
}

impl ReplayGainTags {
    /// `true` when the file carried nothing usable, which is the case
    /// for most untagged libraries. The scanner writes the four
    /// columns either way — they have to be cleared when a tagger
    /// removes a gain — so this is for callers that want to tell
    /// "absent" from "present and zero".
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Read the ReplayGain tags off an already-parsed lofty tag.
///
/// Works through lofty's generic [`Tag`] rather than a concrete
/// format tag — unlike the *write* path, which has to use the
/// concrete type or lose every non-standard comment. These keys are
/// all in lofty's mapping table for ID3v2 `TXXX`, Vorbis comments,
/// APE and MP4 iTunes atoms, so the generic view carries them intact.
pub fn extract_replay_gain(tag: &Tag) -> ReplayGainTags {
    // ReplayGain 2.0 first: it's the common case, and a file that has
    // both is a file whose tagger wrote the R128 pair for Opus players
    // and the textual pair for everyone else.
    let track_gain_db = tag
        .get_string(ItemKey::ReplayGainTrackGain)
        .and_then(parse_gain_db)
        .or_else(|| {
            tag.get_string(ItemKey::R128TrackGain)
                .and_then(parse_r128_gain_db)
        });
    let album_gain_db = tag
        .get_string(ItemKey::ReplayGainAlbumGain)
        .and_then(parse_gain_db)
        .or_else(|| {
            tag.get_string(ItemKey::R128AlbumGain)
                .and_then(parse_r128_gain_db)
        });

    ReplayGainTags {
        track_gain_db,
        track_peak: tag
            .get_string(ItemKey::ReplayGainTrackPeak)
            .and_then(parse_peak),
        album_gain_db,
        album_peak: tag
            .get_string(ItemKey::ReplayGainAlbumPeak)
            .and_then(parse_peak),
    }
}

/// Parse a textual ReplayGain value: a signed decimal, optionally
/// followed by a `dB` unit in any case, with any amount of space.
fn parse_gain_db(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    // Strip a trailing unit without allocating a lowercase copy of
    // the whole string.
    let number = match trimmed.len().checked_sub(2) {
        Some(cut) if trimmed[cut..].eq_ignore_ascii_case("db") => trimmed[..cut].trim_end(),
        _ => trimmed,
    };
    let value: f64 = number.parse().ok()?;
    (value.is_finite() && value.abs() <= MAX_PLAUSIBLE_GAIN_DB).then_some(value)
}

/// Parse an R128 gain: a Q7.8 integer in LU relative to −23 LUFS,
/// returned in dB relative to −18 LUFS so it lines up with everything
/// else.
fn parse_r128_gain_db(raw: &str) -> Option<f64> {
    let q78: i32 = raw.trim().parse().ok()?;
    let value = f64::from(q78) / 256.0 + R128_TO_REPLAY_GAIN_LU;
    (value.abs() <= MAX_PLAUSIBLE_GAIN_DB).then_some(value)
}

/// Parse a linear peak. Zero is rejected along with the negatives: a
/// peak of zero would make the anti-clip limiter compute an infinite
/// headroom, and a genuinely silent file has no gain to limit anyway.
fn parse_peak(raw: &str) -> Option<f64> {
    let value: f64 = raw.trim().parse().ok()?;
    (value.is_finite() && value > 0.0 && value <= MAX_PLAUSIBLE_PEAK).then_some(value)
}

#[cfg(test)]
mod tests;
