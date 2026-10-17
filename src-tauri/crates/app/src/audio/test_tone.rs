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
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn render(rate: u32, channels: usize) -> Vec<f32> {
        let mut buf = vec![0.0; total_frames(rate) as usize * channels + channels * 10];
        let frames = fill(&mut buf, 0, rate, channels);
        assert_eq!(frames as u64, total_frames(rate));
        buf.truncate(frames * channels);
        buf
    }

    fn peak(samples: impl Iterator<Item = f32>) -> f32 {
        samples.fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn the_three_beeps_land_on_the_right_channels() {
        let buf = render(RATE, 2);
        let slot = slot_frames(RATE) as usize;
        let beep = ms_to_frames(BEEP_MS, RATE) as usize;
        let channel = |from: usize, to: usize, ch: usize| {
            peak(buf[from * 2..to * 2].iter().skip(ch).step_by(2).copied())
        };
        // Left beep: left only.
        assert!(channel(0, beep, 0) > 0.1);
        assert_eq!(channel(0, beep, 1), 0.0);
        // Right beep: right only.
        assert_eq!(channel(slot, slot + beep, 0), 0.0);
        assert!(channel(slot, slot + beep, 1) > 0.1);
        // Both.
        assert!(channel(2 * slot, 2 * slot + beep, 0) > 0.1);
        assert!(channel(2 * slot, 2 * slot + beep, 1) > 0.1);
        // The gaps are silent.
        assert_eq!(channel(beep, slot, 0), 0.0);
        assert_eq!(channel(beep, slot, 1), 0.0);
    }

    #[test]
    fn it_never_exceeds_its_level_and_starts_and_ends_at_zero() {
        for rate in [44_100, 48_000, 96_000, 192_000] {
            let buf = render(rate, 2);
            assert!(peak(buf.iter().copied()) <= AMPLITUDE as f32 + 1e-6);
            assert!(buf[0].abs() < 1e-3 && buf[1].abs() < 1e-3);
            let n = buf.len();
            assert!(buf[n - 1].abs() < 1e-2 && buf[n - 2].abs() < 1e-2);
        }
    }

    #[test]
    fn surround_channels_stay_silent_and_mono_gets_every_beep() {
        let buf = render(RATE, 6);
        for ch in 2..6 {
            assert_eq!(peak(buf.iter().skip(ch).step_by(6).copied()), 0.0);
        }
        let mono = render(RATE, 1);
        let slot = slot_frames(RATE) as usize;
        let beep = ms_to_frames(BEEP_MS, RATE) as usize;
        for start in [0, slot, 2 * slot] {
            assert!(peak(mono[start..start + beep].iter().copied()) > 0.1);
        }
    }

    #[test]
    fn the_phase_changes_when_a_beep_starts() {
        let slot = slot_frames(RATE);
        assert_eq!(phase_at(0, RATE), Some(TonePhase::Left));
        assert_eq!(phase_at(slot - 1, RATE), Some(TonePhase::Left));
        assert_eq!(phase_at(slot, RATE), Some(TonePhase::Right));
        assert_eq!(phase_at(2 * slot, RATE), Some(TonePhase::Both));
        assert_eq!(phase_at(total_frames(RATE), RATE), None);
        assert_eq!(phase_at(0, 0), None);
    }

    #[test]
    fn a_fill_resumes_where_the_last_one_stopped() {
        let whole = render(RATE, 2);
        let mut pieces = Vec::new();
        let mut frame = 0u64;
        let mut chunk = vec![0.0; 480 * 2];
        loop {
            let n = fill(&mut chunk, frame, RATE, 2);
            if n == 0 {
                break;
            }
            pieces.extend_from_slice(&chunk[..n * 2]);
            frame += n as u64;
        }
        assert_eq!(pieces, whole);
    }
}
