//! Crossfade DSP helpers: the equal-power gain curves, and the window a
//! fade is mixed over.
//!
//! The track fading in is decoded by a second
//! [`waveflow_core::playback::stream::ActiveStream`], opened to the same
//! output rate and channel count as the primary, and the decoder mixes the
//! two with these gains.

/// Equal-power fade gains for a normalized progress `t ∈ [0, 1]`.
/// Returns `(fade_out_gain, fade_in_gain)` where
/// `fade_out² + fade_in² = 1` so total power is preserved across the
/// mix — gives a noticeably less "scooped" mid-fade than linear gains.
#[inline]
pub fn equal_power_gains(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let angle = t * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

/// How long before an AutoMix fade starts the decoder enters the mix, in
/// microseconds of the outgoing track. The mix is entered between two
/// packets, never on the planned frame itself; until that frame the
/// outgoing track plays alone through the mix at full gain. Half a second
/// is a few packets of any codec, and short enough that a seek or an A-B
/// jump just before the fade still finds the track playing normally.
pub const AUTOMIX_ENTER_US: u64 = 500_000;

/// One crossfade, in output frames counted from the first one mixed.
///
/// A fixed crossfade ramps from its first frame. An AutoMix one is
/// entered a little early ([`AUTOMIX_ENTER_US`]) and lands its ramp and
/// the incoming track's first frame on the frames its plan placed on the
/// beats ([`MixWindow::automix`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixWindow {
    /// Frames of the outgoing track alone, at full gain, before the ramp.
    lead: u64,
    /// The frame the incoming track's first output frame plays on.
    incoming_at: u64,
    /// Frames the ramp lasts, from `lead`.
    fade: u64,
    /// Frames mixed so far.
    written: u64,
}

impl MixWindow {
    /// The plain crossfade: the ramp and the incoming track from the
    /// first frame.
    pub fn fixed(fade_frames: u64) -> Self {
        Self {
            lead: 0,
            incoming_at: 0,
            fade: fade_frames,
            written: 0,
        }
    }

    /// An AutoMix plan, from where the outgoing track stands.
    ///
    /// `now_us` is the outgoing track's position as the clock counts it
    /// for the next frame the decoder produces: the frames written since
    /// the clock started, so with its resampler's delay in
    /// (`primary_delay`, output frames) — a source frame comes out that
    /// many frames after the count says. The plan's times are the track's
    /// own. `fade_start_us` is where the incoming track starts, the ramp
    /// with it; `fade_end_us` where it ends.
    ///
    /// The incoming track's own resampler delay is not known before its
    /// first packet: [`Self::start_incoming_earlier`] takes it then.
    ///
    /// `None` when the fade should already have started.
    pub fn automix(
        fade_start_us: u64,
        fade_end_us: u64,
        now_us: u64,
        rate: u32,
        speed: f32,
        primary_delay: u64,
    ) -> Option<Self> {
        if fade_start_us < now_us || fade_end_us <= fade_start_us {
            return None;
        }
        let frames = |us: u64| {
            (us as f64 / 1e6 * f64::from(rate) / f64::from(speed.max(0.01))).round() as u64
        };
        let lead = frames(fade_start_us - now_us) + primary_delay;
        Some(Self {
            lead,
            incoming_at: lead,
            fade: frames(fade_end_us - fade_start_us).max(1),
            written: 0,
        })
    }

    /// Start the incoming track `frames` earlier: its resampler puts that
    /// many frames of delay ahead of its first sample. Never before what
    /// has already been mixed.
    pub fn start_incoming_earlier(&mut self, frames: u64) {
        self.incoming_at = self.incoming_at.saturating_sub(frames).max(self.written);
    }

    /// Frames in the whole window, lead included.
    pub fn total(&self) -> u64 {
        self.lead + self.fade
    }

    /// Whether every frame of the window has been mixed.
    pub fn done(&self) -> bool {
        self.written >= self.total()
    }

    /// Frames still to mix before the incoming track's first one.
    pub fn incoming_wait(&self) -> u64 {
        self.incoming_at.saturating_sub(self.written)
    }

    /// Gains for frame `frame` of the window: the outgoing track alone
    /// through the lead, then the equal-power ramp.
    fn gains_at(&self, frame: u64) -> (f32, f32) {
        if frame < self.lead {
            return (1.0, 0.0);
        }
        equal_power_gains((frame - self.lead) as f32 / self.fade.max(1) as f32)
    }

    /// Mix one pass of `primary` and `secondary` (interleaved, `channels`
    /// per frame) into `out`, which is cleared first. Returns how many
    /// frames each side gave: the caller drains that much from each.
    ///
    /// The incoming track contributes nothing before its first frame
    /// ([`Self::incoming_wait`] counts as frames it already holds, so a
    /// pass is not held back waiting for audio it does not need yet), and
    /// a side that has ended counts as silence ([`crossfade_mix_frames`]).
    pub fn mix(
        &mut self,
        primary: &[f32],
        secondary: &[f32],
        primary_at_eof: bool,
        secondary_at_eof: bool,
        channels: usize,
        out: &mut Vec<f32>,
    ) -> (usize, usize) {
        let channels = channels.max(1);
        let primary_frames = primary.len() / channels;
        let secondary_frames = secondary.len() / channels;
        let wait = self.incoming_wait() as usize;
        let remaining = self.total().saturating_sub(self.written) as usize;
        let frames = crossfade_mix_frames(
            primary_frames,
            secondary_frames.saturating_add(wait),
            primary_at_eof,
            secondary_at_eof,
            remaining,
        );
        out.clear();
        out.reserve(frames * channels);
        for f in 0..frames {
            let (g_out, g_in) = self.gains_at(self.written);
            let incoming = f.checked_sub(wait).filter(|&i| i < secondary_frames);
            for ch in 0..channels {
                // 0 for primary frames past the buffer — happens only when
                // the primary has ended and the rest of the fade runs out.
                let p = if f < primary_frames {
                    primary[f * channels + ch] * g_out
                } else {
                    0.0
                };
                // And 0 for the incoming track before its first frame,
                // and once it has run out when shorter than the window.
                let s = incoming.map_or(0.0, |i| secondary[i * channels + ch] * g_in);
                out.push(p + s);
            }
            self.written += 1;
        }
        (
            frames.min(primary_frames),
            frames.saturating_sub(wait).min(secondary_frames),
        )
    }
}

/// How many frames a crossfade mixes in one pass.
///
/// A side that has reached its end counts as silence for the rest of the
/// window, either way round. The outgoing track can end a little before
/// the duration the fade was timed against (DSD in particular); the
/// incoming one can be shorter than the window altogether, since the
/// window is timed against the outgoing track (a jingle under a long
/// fade). Before the second case was handled, an incoming track that had
/// run out left nothing to mix and the loop waited for frames that never
/// came, with the outgoing track still playing: playback stalled. Now the
/// outgoing track finishes its fade, then the queue moves on.
///
/// While neither side has ended, only what both hold is mixed: the next
/// pass decodes more of the shorter one. Never past the window: an AutoMix
/// fade ends before the outgoing track does, and what is left of it is
/// not part of the mix.
pub fn crossfade_mix_frames(
    primary_frames: usize,
    secondary_frames: usize,
    primary_at_eof: bool,
    secondary_at_eof: bool,
    remaining_window: usize,
) -> usize {
    match (primary_at_eof, secondary_at_eof) {
        (false, false) => primary_frames.min(secondary_frames).min(remaining_window),
        (true, false) => secondary_frames.min(remaining_window),
        (false, true) => primary_frames.min(remaining_window),
        (true, true) => primary_frames.max(secondary_frames).min(remaining_window),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod alignment_tests;
