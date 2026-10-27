//! The output test tone: a 1 kHz beep on the left channel, then on the
//! right, then on both, generated at whatever rate and channel count the
//! output stream runs at.
//!
//! Generated rather than decoded from a bundled file, so it reaches the
//! device at the stream's own format with nothing in between: no
//! resampler, no ReplayGain, no EQ. Volume, normalize and mono still
//! apply, because the output applies them to everything it plays.
//!
//! Pure: [`fill`] and [`phase_at`] only do arithmetic on frame numbers,
//! and the decoder thread owns the loop that pushes the result.

use serde::Serialize;

/// Pitch of the beep. 1 kHz is the reference tone of audio test gear,
/// and sits where the ear is sensitive without being piercing.
const FREQ_HZ: f64 = 1000.0;

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
}

/// The beeps in order. A gap belongs to the beep before it, so the
/// phase on screen changes when the next beep starts and not before.
const SEQUENCE: [TonePhase; 3] = [TonePhase::Left, TonePhase::Right, TonePhase::Both];

fn ms_to_frames(ms: u64, rate: u32) -> u64 {
    ms * u64::from(rate) / 1000
}

/// Frames in one beep plus the gap after it.
fn slot_frames(rate: u32) -> u64 {
    ms_to_frames(BEEP_MS + GAP_MS, rate)
}

/// Length of the whole tone, in frames. The last beep has no gap after it.
pub fn total_frames(rate: u32) -> u64 {
    slot_frames(rate) * (SEQUENCE.len() as u64 - 1) + ms_to_frames(BEEP_MS, rate)
}

/// The phase sounding at `frame`, `None` once the tone is over.
pub fn phase_at(frame: u64, rate: u32) -> Option<TonePhase> {
    if rate == 0 || frame >= total_frames(rate) {
        return None;
    }
    let slot = (frame / slot_frames(rate)) as usize;
    SEQUENCE.get(slot).copied()
}

/// Level of one mono sample of the tone at `frame`: the sine, shaped by
/// the ramps, and silence in the gaps.
fn level_at(frame: u64, rate: u32) -> f32 {
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
    (AMPLITUDE * envelope * (2.0 * std::f64::consts::PI * FREQ_HZ * t).sin()) as f32
}

/// Fill `buf` — interleaved, `channels` per frame — with the tone from
/// `start_frame` on, and return how many frames it wrote: fewer than
/// the buffer holds once the tone ends.
///
/// Left is channel 0 and right channel 1, the order every backend
/// opens a stereo pair in. Any further channel stays silent: a test of
/// left and right says nothing about a centre or a surround speaker. A
/// single-channel output plays all three beeps on that channel.
pub fn fill(buf: &mut [f32], start_frame: u64, rate: u32, channels: usize) -> usize {
    if channels == 0 || rate == 0 {
        return 0;
    }
    let mut written = 0;
    for (i, frame) in buf.chunks_exact_mut(channels).enumerate() {
        let at = start_frame + i as u64;
        let Some(phase) = phase_at(at, rate) else {
            break;
        };
        frame.fill(0.0);
        let level = level_at(at, rate);
        if channels == 1 {
            frame[0] = level;
        } else {
            match phase {
                TonePhase::Left => frame[0] = level,
                TonePhase::Right => frame[1] = level,
                TonePhase::Both => {
                    frame[0] = level;
                    frame[1] = level;
                }
            }
        }
        written += 1;
    }
    written
}

#[cfg(test)]
mod tests;
