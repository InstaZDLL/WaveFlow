//! The output test tone: a 1 kHz beep on the left channel, then on the
//! right, then on both, generated at whatever rate and channel count the
//! output stream runs at. On a surround output, a beep on each speaker in
//! turn instead, since that is the only way to hear whether each channel
//! comes out of the speaker it should.
//!
//! Generated rather than decoded from a bundled file, so it reaches the
//! device at the stream's own format with nothing in between: no
//! resampler, no ReplayGain, no EQ. Volume, normalize and mono still
//! apply, because the output applies them to everything it plays.
//!
//! Pure: [`ToneSequence::fill`] and [`ToneSequence::phase_at`] only do
//! arithmetic on frame numbers, and the decoder thread owns the loop that
//! pushes the result.

use serde::Serialize;
use waveflow_core::playback::channels::{ChannelLayout, Speaker};

/// Pitch of the beep. 1 kHz is the reference tone of audio test gear,
/// and sits where the ear is sensitive without being piercing.
const FREQ_HZ: f64 = 1000.0;

/// Pitch of the LFE's beep. A subwoofer is crossed over far below 1 kHz, and
/// would play that beep as silence.
const LFE_FREQ_HZ: f64 = 60.0;

/// −18 dBFS: clearly audible, far from a full-scale blast at a volume
/// set for music, which has peaks near 0 dBFS.
const AMPLITUDE: f64 = 0.125_892_54;

/// Length of each beep and of the silence after it, in milliseconds.
const BEEP_MS: u64 = 800;
const GAP_MS: u64 = 300;

/// Raised-cosine ramp at both ends of a beep, so it starts and stops
/// without a click.
const RAMP_MS: u64 = 15;

/// Which part of the tone is sounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TonePhase {
    Left,
    Right,
    Both,
    Center,
    Lfe,
    BackLeft,
    BackRight,
    BackCenter,
    SideLeft,
    SideRight,
}

/// The beeps in order. A gap belongs to the beep before it, so the
/// phase on screen changes when the next beep starts and not before.
const STEREO_SEQUENCE: [TonePhase; 3] = [TonePhase::Left, TonePhase::Right, TonePhase::Both];

/// Which of the surround phases each speaker sounds under, in the order a
/// speaker test walks a room: the front, the LFE, the back, then the sides.
const SURROUND_SEQUENCE: [(Speaker, TonePhase); 9] = [
    (Speaker::FrontLeft, TonePhase::Left),
    (Speaker::FrontRight, TonePhase::Right),
    (Speaker::FrontCenter, TonePhase::Center),
    (Speaker::Lfe, TonePhase::Lfe),
    (Speaker::BackLeft, TonePhase::BackLeft),
    (Speaker::BackRight, TonePhase::BackRight),
    (Speaker::BackCenter, TonePhase::BackCenter),
    (Speaker::SideLeft, TonePhase::SideLeft),
    (Speaker::SideRight, TonePhase::SideRight),
];

fn ms_to_frames(ms: u64, rate: u32) -> u64 {
    ms * u64::from(rate) / 1000
}

/// Frames in one beep plus the gap after it.
fn slot_frames(rate: u32) -> u64 {
    ms_to_frames(BEEP_MS + GAP_MS, rate)
}

/// Level of one mono sample of the tone at `frame`: the sine, shaped by
/// the ramps, and silence in the gaps.
fn level_at(frame: u64, rate: u32, freq_hz: f64) -> f32 {
    let slot = slot_frames(rate);
    let within = frame % slot;
    let beep = ms_to_frames(BEEP_MS, rate);
    if within >= beep {
        return 0.0;
    }
    let ramp = ms_to_frames(RAMP_MS, rate).max(1);
    let edge = within.min(beep - 1 - within);
    let envelope = if edge < ramp {
        let x = edge as f64 / ramp as f64;
        0.5 - 0.5 * (std::f64::consts::PI * x).cos()
    } else {
        1.0
    };
    let t = frame as f64 / f64::from(rate);
    (AMPLITUDE * envelope * (2.0 * std::f64::consts::PI * freq_hz * t).sin()) as f32
}

/// One beep: what the card calls it, and the channels it sounds on.
#[derive(Debug, Clone)]
struct Beep {
    phase: TonePhase,
    channels: Vec<usize>,
    freq_hz: f64,
}

/// The beeps an output with a given layout gets.
#[derive(Debug, Clone)]
pub struct ToneSequence {
    channels: usize,
    beeps: Vec<Beep>,
}

impl ToneSequence {
    /// Left, right and both on a stereo output; on a wider one, every speaker
    /// of a 5.1 or a 7.1 that it has, found by name rather than number since
    /// some backends open a surround output in another order. The front pair
    /// of a 7.1 wide, which only an obsolete mask gives, is not called on. A
    /// single-channel output plays all
    /// three stereo beeps on that channel.
    ///
    /// `front_only` is for the mono switch, which plays from the front pair
    /// alone: the stereo beeps then, whatever the width.
    pub fn new(layout: &ChannelLayout, front_only: bool) -> Self {
        let channels = layout.channels();
        let beep = |phase, channels: Vec<usize>, freq_hz| Beep {
            phase,
            channels,
            freq_hz,
        };
        let beeps = match channels {
            0 => Vec::new(),
            1 => STEREO_SEQUENCE
                .iter()
                .map(|phase| beep(*phase, vec![0], FREQ_HZ))
                .collect(),
            n if n == 2 || (front_only && layout.front_pair().is_some()) => {
                let (left, right) = layout.front_pair().unwrap_or((0, 1));
                vec![
                    beep(TonePhase::Left, vec![left], FREQ_HZ),
                    beep(TonePhase::Right, vec![right], FREQ_HZ),
                    beep(TonePhase::Both, vec![left, right], FREQ_HZ),
                ]
            }
            _ => SURROUND_SEQUENCE
                .iter()
                .filter_map(|(speaker, phase)| {
                    let channel = layout.speakers().iter().position(|s| s == speaker)?;
                    let freq = if *speaker == Speaker::Lfe {
                        LFE_FREQ_HZ
                    } else {
                        FREQ_HZ
                    };
                    Some(beep(*phase, vec![channel], freq))
                })
                .collect(),
        };
        Self { channels, beeps }
    }

    /// The beeps to come, in order.
    pub fn phases(&self) -> Vec<TonePhase> {
        self.beeps.iter().map(|b| b.phase).collect()
    }

    /// Length of the whole tone, in frames. The last beep has no gap after it.
    pub fn total_frames(&self, rate: u32) -> u64 {
        match self.beeps.len() as u64 {
            0 => 0,
            beeps => slot_frames(rate) * (beeps - 1) + ms_to_frames(BEEP_MS, rate),
        }
    }

    fn beep_at(&self, frame: u64, rate: u32) -> Option<&Beep> {
        if rate == 0 || frame >= self.total_frames(rate) {
            return None;
        }
        self.beeps.get((frame / slot_frames(rate)) as usize)
    }

    /// The phase sounding at `frame`, `None` once the tone is over.
    pub fn phase_at(&self, frame: u64, rate: u32) -> Option<TonePhase> {
        self.beep_at(frame, rate).map(|b| b.phase)
    }

    /// Fill `buf` — interleaved, as wide as the layout — with the tone from
    /// `start_frame` on, and return how many frames it wrote: fewer than
    /// the buffer holds once the tone ends.
    pub fn fill(&self, buf: &mut [f32], start_frame: u64, rate: u32) -> usize {
        if self.channels == 0 || rate == 0 {
            return 0;
        }
        let mut written = 0;
        for (i, frame) in buf.chunks_exact_mut(self.channels).enumerate() {
            let at = start_frame + i as u64;
            let Some(beep) = self.beep_at(at, rate) else {
                break;
            };
            frame.fill(0.0);
            let level = level_at(at, rate, beep.freq_hz);
            for channel in &beep.channels {
                frame[*channel] = level;
            }
            written += 1;
        }
        written
    }
}

#[cfg(test)]
mod tests;
