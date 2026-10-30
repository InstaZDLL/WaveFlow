//! AutoMix: a crossfade placed on the beats of both tracks.
//!
//! The plain crossfade starts a fixed time before the end and lets the
//! two rhythms land where they land. AutoMix reads the beat grids the
//! analysis stored ([`crate::analysis::beats`]) and plans the transition
//! from them:
//!
//! - **Where the outgoing track leaves.** One beat after its last beat.
//!   The beat list is already trimmed where the beats fall to under half
//!   their typical strength ([`crate::analysis::beats`]'s `trim`), so its
//!   end is where the music thins out — an outro, a fade-out, the silence
//!   before the end of the file — rather than the end of the file itself.
//! - **How long the fade lasts.** A whole number of the outgoing track's
//!   beats — 32, 16, 8 or 4, the longest the user's crossfade length
//!   holds — counted from the incoming track's first beat, so the fade
//!   stays within the seconds the user set. A crossfade setting too short
//!   for 4 beats gets 2 or 1.
//! - **Where the incoming track starts.** From its first sample, as it
//!   always does, at the moment that puts one of its beats on one of the
//!   outgoing track's, in the middle of the fade where both are loudest.
//!   Its intro, up to its first beat, plays under the end of the outgoing
//!   track as part of the fade.
//!
//! **There is no time-stretching** (not before 2.1), so two tempos that
//! differ drift apart over the fade. The drift is predicted from the two
//! grids, and the fade is shortened until its ends drift no more than
//! [`MAX_DRIFT_MS`] from the anchor; a pair whose tempos are too far apart
//! for even a 4-beat fade gets no plan. A tempo double or half the other
//! counts as the same one: the slower track's beats land on every other
//! beat of the faster one.
//!
//! [`plan`] returns why it gave up when it does ([`Fallback`]); the player
//! then makes the transition it makes without AutoMix.

pub mod harmonic;

/// Beats needed at either end of a track to fit a grid: two bars of 4/4.
pub const MIN_GRID_BEATS: usize = 8;

/// Beats the grid is fitted on: the last ones of the outgoing track, the
/// first ones of the incoming track. Eight bars: enough that a beat the
/// tracker placed a little early or late moves the line by a fraction of
/// a millisecond per beat, few enough that a slow tempo change over the
/// track does not bend it.
pub const FIT_BEATS: usize = 32;

/// How far, as a share of the period, the beats may scatter around the
/// fitted line before the grid is not trusted. A tracker that slipped
/// half a beat for part of the run, or a tempo that moves, leaves the
/// beats scattered by a quarter of a period and more; a steady grid with
/// the tracker's usual jitter (10 to 20 ms) stays under a twentieth.
pub const MAX_SCATTER: f64 = 0.1;

/// The furthest the two tracks' beats may drift apart at the ends of the
/// fade, in milliseconds. Two attacks closer than about 20 ms fuse into
/// one; a few tens of milliseconds apart they are heard as a flam, then as
/// two beats. At the ends of the fade one track is the quieter by far, so
/// 30 ms there leaves the middle — where both are loud — closer still.
///
/// This is what sets the tempo gap AutoMix accepts. With the beats lined
/// up in the middle, a fade of `n` beats drifts `n / 2` periods times the
/// tempo gap at its ends, so a 4-beat fade (the shortest kept) holds
/// `30 ms / (2 × period)`: 3 % at 120 BPM, 2 % at 80, 4.3 % at 174.
/// Longer fades need closer tempos (16 beats at 120 BPM: 0.75 %).
pub const MAX_DRIFT_MS: f64 = 30.0;

/// The earliest the outgoing track may be left: this long before its end.
/// A track whose beats stop earlier than that has a long section without
/// them — a coda, a hidden track after a silence — that a fade from its
/// last beat would cut.
pub const MAX_EARLY_EXIT_MS: u64 = 30_000;

/// Fade lengths tried, in outgoing beats, longest first.
const FADE_BEATS: [u32; 6] = [32, 16, 8, 4, 2, 1];

/// The shortest fade that still counts as a mix of the two rhythms: one
/// bar. Shorter is only taken when the crossfade setting leaves no room
/// for it.
const MIN_MIX_BEATS: u32 = 4;

/// A track as AutoMix sees it: where its beats fall, in seconds from its
/// start, and how long it is.
#[derive(Debug, Clone, Copy)]
pub struct TrackBeats<'a> {
    pub beats: &'a [f64],
    pub duration_ms: u64,
}

/// A run of beats as a straight line: beat `k` of the run falls at
/// `origin + k × period` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub origin: f64,
    pub period: f64,
    /// Root-mean-square distance of the beats from the line, in seconds.
    pub scatter: f64,
}

impl Grid {
    /// Least-squares line through `beats`, taken as consecutive beats.
    /// `None` under two beats or for a line that does not go forward.
    pub fn fit(beats: &[f64]) -> Option<Self> {
        let n = beats.len();
        if n < 2 {
            return None;
        }
        let nf = n as f64;
        let mean_k = (nf - 1.0) / 2.0;
        let mean_t = beats.iter().sum::<f64>() / nf;
        let mut num = 0.0;
        let mut den = 0.0;
        for (k, &t) in beats.iter().enumerate() {
            let dk = k as f64 - mean_k;
            num += dk * (t - mean_t);
            den += dk * dk;
        }
        let period = num / den;
        if !period.is_finite() || period <= 0.0 {
            return None;
        }
        let origin = mean_t - period * mean_k;
        let scatter = (beats
            .iter()
            .enumerate()
            .map(|(k, &t)| (t - origin - period * k as f64).powi(2))
            .sum::<f64>()
            / nf)
            .sqrt();
        Some(Self {
            origin,
            period,
            scatter,
        })
    }

    /// Where beat `k` of the run falls, in seconds.
    pub fn at(&self, k: f64) -> f64 {
        self.origin + self.period * k
    }

    fn is_steady(&self) -> bool {
        self.scatter <= MAX_SCATTER * self.period
    }
}

/// Why a transition gets no plan, so the player falls back to the
/// crossfade it makes without AutoMix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fallback {
    /// One of the tracks has fewer than [`MIN_GRID_BEATS`] beats (no
    /// grid stored, or too few to fit one).
    NoGrid,
    /// One of the grids scatters more than [`MAX_SCATTER`] of a period.
    UnsteadyGrid,
    /// The outgoing track's beats stop more than [`MAX_EARLY_EXIT_MS`]
    /// before its end.
    LongOutro,
    /// The incoming track's first beat comes too late for its intro and
    /// a beat of fade to fit in the crossfade length.
    LongIntro,
    /// Not even one beat fits in the crossfade length, or in half of the
    /// outgoing track.
    NoRoom,
    /// The tempos (folded by a factor of two) are too far apart for a
    /// 4-beat fade to stay within [`MAX_DRIFT_MS`]. `ratio` is the
    /// incoming period over the outgoing one, after folding.
    TempoMismatch { ratio: f64 },
}

/// A transition placed on the beats. Every time is in the outgoing
/// track's own time, in microseconds from its start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// When the incoming track's first sample plays, and the fade starts
    /// with it.
    pub fade_start_us: u64,
    /// When the fade ends, the outgoing track silent from then on.
    pub fade_end_us: u64,
    /// The fade's length in outgoing beats, from the incoming track's
    /// first beat to the end.
    pub beats: u32,
    /// How many outgoing beats one incoming beat spans: 1, 2 (the
    /// incoming track runs at half the tempo) or 0.5 (at double).
    pub fold: f64,
    /// Incoming period over the outgoing one, after folding: 1.0 is the
    /// same tempo.
    pub ratio: f64,
    /// The furthest apart the two grids put a pair of beats over the
    /// fade, in milliseconds.
    pub max_drift_ms: f64,
}

/// The outgoing side of a plan: the grid over its last beats, how many
/// beats it was fitted on, and the moment the fade ends, in seconds.
fn outgoing(out: TrackBeats<'_>) -> Result<(Grid, usize, f64), Fallback> {
    if out.beats.len() < MIN_GRID_BEATS || out.duration_ms == 0 {
        return Err(Fallback::NoGrid);
    }
    let run = &out.beats[out.beats.len().saturating_sub(FIT_BEATS)..];
    let grid = Grid::fit(run).ok_or(Fallback::NoGrid)?;
    if !grid.is_steady() {
        return Err(Fallback::UnsteadyGrid);
    }
    let duration = out.duration_ms as f64 / 1000.0;
    // One beat past the last one: the last beat still sounds as the
    // fade closes over it.
    let end = grid.at(run.len() as f64).min(duration);
    if end * 1000.0 + (MAX_EARLY_EXIT_MS as f64) < duration * 1000.0 {
        return Err(Fallback::LongOutro);
    }
    Ok((grid, run.len(), end))
}

/// The earliest moment, in milliseconds of the outgoing track, a plan
/// for it can start its fade, whatever track comes next: a fade never
/// lasts longer than `cap_ms`. The player asks for the next track before
/// this, so the plan is ready in time. `None` when the outgoing track
/// has no grid AutoMix can use; the player then asks when it always has.
pub fn earliest_fade_start_ms(out: TrackBeats<'_>, cap_ms: u64) -> Option<u64> {
    let (_, _, end) = outgoing(out).ok()?;
    let earliest = end - cap_ms as f64 / 1000.0;
    Some((earliest.max(0.0) * 1000.0) as u64)
}

/// Plan the transition from `out` to `inc` within a crossfade of
/// `cap_ms`.
pub fn plan(out: TrackBeats<'_>, inc: TrackBeats<'_>, cap_ms: u64) -> Result<Plan, Fallback> {
    let (og, run_len, end) = outgoing(out)?;
    if inc.beats.len() < MIN_GRID_BEATS {
        return Err(Fallback::NoGrid);
    }
    let ig = Grid::fit(&inc.beats[..inc.beats.len().min(FIT_BEATS)]).ok_or(Fallback::NoGrid)?;
    if !ig.is_steady() {
        return Err(Fallback::UnsteadyGrid);
    }

    // Fold the incoming period onto the outgoing one: whichever of
    // half, same or double is nearest on a log scale.
    let q = ig.period / og.period;
    let fold = [0.5, 1.0, 2.0]
        .into_iter()
        .min_by(|a: &f64, b: &f64| (q / a).ln().abs().total_cmp(&(q / b).ln().abs()))
        .unwrap_or(1.0);
    let ratio = q / fold;

    // Never longer than the setting, nor than half the outgoing track
    // (the plain crossfade's own bound).
    let cap = (cap_ms as f64 / 1000.0).min(out.duration_ms as f64 / 2000.0);
    // The outgoing grid's line `run_len` is `end` (unless the file ends
    // first); the fade's lines count back from there.
    let end_line = run_len as f64;

    let mut fits_cap = false;
    let mut longest_fitting: Option<u32> = None;
    for beats in FADE_BEATS {
        let n = f64::from(beats);
        if og.at(end_line - n) < 0.0 {
            continue;
        }
        // The anchor: the outgoing line nearest the middle of the fade
        // that an incoming beat can land on (every other line when the
        // incoming track runs at half the tempo).
        let step = fold.max(1.0);
        let anchor_lines = ((n / 2.0) / step).round() * step;
        let anchor = og.at(end_line - n + anchor_lines);
        // The incoming track is `offset` seconds behind the outgoing
        // one: that is when its first sample plays.
        let offset = anchor - ig.at(anchor_lines / fold);
        let fade_start = offset.min(end);
        if fade_start < 0.0 || end - fade_start > cap + 1e-9 {
            continue;
        }
        fits_cap = true;
        let longest = *longest_fitting.get_or_insert(beats);
        // Below one bar only when the setting is what forbids one.
        if beats < MIN_MIX_BEATS && longest >= MIN_MIX_BEATS {
            break;
        }
        let drift = max_drift(&og, &ig, end_line - n, beats, fold, anchor_lines, offset);
        if drift * 1000.0 <= MAX_DRIFT_MS {
            return Ok(Plan {
                fade_start_us: (fade_start * 1e6).round() as u64,
                fade_end_us: (end * 1e6).round() as u64,
                beats,
                fold,
                ratio,
                max_drift_ms: drift * 1000.0,
            });
        }
    }
    if fits_cap {
        return Err(Fallback::TempoMismatch { ratio });
    }
    // Nothing fits the length: the intro, or the beat itself, is too long.
    if og.period > cap {
        Err(Fallback::NoRoom)
    } else {
        Err(Fallback::LongIntro)
    }
}

/// The furthest apart the two grids put a pair of beats that should
/// coincide, over the `beats` outgoing lines from `first_line`, with the
/// incoming track `offset` seconds behind and the pair `anchor_lines`
/// into the fade lined up. Each line's partner is counted from the
/// anchor, never looked up as the nearest incoming beat: a drift past
/// half a period would otherwise find the next beat and read as small.
/// A line before the incoming track's first beat, or between two of its
/// beats when it runs at half the tempo, has no partner.
fn max_drift(
    og: &Grid,
    ig: &Grid,
    first_line: f64,
    beats: u32,
    fold: f64,
    anchor_lines: f64,
    offset: f64,
) -> f64 {
    let anchor_beat = anchor_lines / fold;
    let mut worst: f64 = 0.0;
    for k in 0..=beats {
        let j = anchor_beat + (f64::from(k) - anchor_lines) / fold;
        if j < 0.0 || j.fract() != 0.0 {
            continue;
        }
        let line = og.at(first_line + f64::from(k));
        worst = worst.max((ig.at(j) + offset - line).abs());
    }
    worst
}

#[cfg(test)]
mod tests;
