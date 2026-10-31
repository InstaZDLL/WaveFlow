//! The AutoMix alignment, measured end to end: two click tracks written
//! as WAV files, planned from their beat lists, decoded by the player's
//! own `ActiveStream` (resampler included) and mixed by [`MixWindow`] the
//! way the decoder loop drives it. The outgoing track clicks on the left
//! channel, the incoming one on the right, so the mix says where each
//! one's beats landed.

use std::path::Path;

use waveflow_core::analysis::beats;
use waveflow_core::automix::{self, TrackBeats};
use waveflow_core::playback::replay_gain::TrackGain;
use waveflow_core::playback::stream::ActiveStream;

use super::{MixWindow, AUTOMIX_ENTER_US};

const OUT_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// The decoder's own top-up target, `CROSSFADE_MIN_FRAMES`.
const MIN_FRAMES: usize = 1024;

/// A stereo 16-bit WAV of `seconds` at `rate`, a 2 ms click at each of
/// `clicks` (seconds) on `channel`, silence elsewhere.
fn click_track(path: &Path, rate: u32, seconds: f64, clicks: &[f64], channel: usize) {
    let frames = (seconds * f64::from(rate)) as usize;
    let mut pcm = vec![0i16; frames * CHANNELS];
    let click_len = (0.002 * f64::from(rate)) as usize;
    for &c in clicks {
        let at = (c * f64::from(rate)).round() as usize;
        for i in 0..click_len {
            if let Some(slot) = pcm.get_mut((at + i) * CHANNELS + channel) {
                // A decaying burst: a sharp attack the peak search finds.
                let env = 1.0 - i as f64 / click_len as f64;
                *slot = (20_000.0 * env) as i16;
            }
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * CHANNELS as u32 * 2).to_le_bytes());
    bytes.extend_from_slice(&((CHANNELS * 2) as u16).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn clicks(bpm: f64, first: f64, until: f64) -> Vec<f64> {
    let period = 60.0 / bpm;
    let mut out = Vec::new();
    let mut t = first;
    while t < until {
        out.push(t);
        t += period;
    }
    out
}

/// Through the stored encoding, as the analysis row would hand them over.
fn stored(beats_s: &[f64]) -> Vec<f64> {
    beats::decode(&beats::encode(beats_s).unwrap()).unwrap()
}

fn open(path: &Path, id: i64, duration_ms: u64) -> ActiveStream {
    ActiveStream::open(
        path,
        id,
        duration_ms,
        "manual".into(),
        None,
        TrackGain::default(),
        0,
        false,
    )
    .unwrap()
}

/// What came out of the mix, and where the fade sat in it.
struct Mixed {
    out: Vec<f32>,
    /// Output frame of the incoming track's first frame, as planned.
    incoming_frame: usize,
    fade_end_frame: usize,
}

/// Play `out_path` from its start and mix `in_path` into it on `plan`,
/// as the decoder loop does: whole packets until the mix is due, then
/// [`MixWindow`] passes over buffers topped up to [`MIN_FRAMES`].
fn run(out_path: &Path, out_ms: u64, in_path: &Path, in_ms: u64, plan: &automix::Plan) -> Mixed {
    let mut primary = open(out_path, 1, out_ms);
    let mut secondary = open(in_path, 2, in_ms);
    let mut scratch = Vec::new();
    let mut out: Vec<f32> = Vec::new();

    // Until the mix is due: one packet a pass, pushed whole.
    let mut buf = Vec::new();
    loop {
        let now_us = (out.len() / CHANNELS) as u64 * 1_000_000 / u64::from(OUT_RATE);
        if now_us + AUTOMIX_ENTER_US >= plan.fade_start_us {
            break;
        }
        buf.clear();
        assert!(!primary
            .decode_next(&mut buf, &mut scratch, OUT_RATE, CHANNELS)
            .unwrap());
        out.extend_from_slice(&buf);
    }
    let entered_at = out.len() / CHANNELS;
    let now_us = entered_at as u64 * 1_000_000 / u64::from(OUT_RATE);
    let mut window = MixWindow::automix(
        plan.fade_start_us,
        plan.fade_end_us,
        now_us,
        OUT_RATE,
        1.0,
        primary.resampler.output_delay_frames(),
    )
    .expect("entered before the fade");
    let incoming_frame = entered_at + window.incoming_wait() as usize;
    let fade_end_frame = entered_at + window.total() as usize;

    let (mut p_buf, mut s_buf, mut mixed) = (Vec::new(), Vec::new(), Vec::new());
    let (mut p_eof, mut s_eof, mut delay_taken) = (false, false, false);
    while !window.done() {
        while !p_eof && p_buf.len() / CHANNELS < MIN_FRAMES {
            p_eof = primary
                .decode_next(&mut p_buf, &mut scratch, OUT_RATE, CHANNELS)
                .unwrap();
        }
        while !s_eof && s_buf.len() / CHANNELS < MIN_FRAMES {
            s_eof = secondary
                .decode_next(&mut s_buf, &mut scratch, OUT_RATE, CHANNELS)
                .unwrap();
            if !delay_taken {
                window.start_incoming_earlier(secondary.resampler.output_delay_frames());
                delay_taken = true;
            }
        }
        let (p_used, s_used) = window.mix(&p_buf, &s_buf, p_eof, s_eof, CHANNELS, &mut mixed);
        p_buf.drain(..p_used * CHANNELS);
        s_buf.drain(..s_used * CHANNELS);
        out.extend_from_slice(&mixed);
        if mixed.is_empty() && p_eof && s_eof {
            break;
        }
    }
    Mixed {
        out,
        incoming_frame,
        fade_end_frame,
    }
}

/// Frame of the loudest sample of `channel` within `radius` frames of
/// `around`, if any rises above the floor.
fn peak(out: &[f32], channel: usize, around: f64, radius: usize) -> Option<usize> {
    let frames = out.len() / CHANNELS;
    let centre = around.round().max(0.0) as usize;
    let (lo, hi) = (centre.saturating_sub(radius), (centre + radius).min(frames));
    (lo..hi)
        .map(|f| (f, out[f * CHANNELS + channel].abs()))
        .filter(|(_, v)| *v > 1e-3)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(f, _)| f)
}

/// For every incoming click heard inside the fade, how far in ms it lands
/// from the nearest outgoing click. Returns the largest, and how many
/// pairs were measured.
fn measure(mixed: &Mixed, in_clicks: &[f64]) -> (f64, usize) {
    let radius = (0.06 * f64::from(OUT_RATE)) as usize;
    let mut worst: f64 = 0.0;
    let mut pairs = 0;
    for &c in in_clicks {
        let expected = mixed.incoming_frame as f64 + c * f64::from(OUT_RATE);
        if expected >= mixed.fade_end_frame as f64 - radius as f64 {
            break;
        }
        let Some(r) = peak(&mixed.out, 1, expected, radius) else {
            continue;
        };
        let Some(l) = peak(&mixed.out, 0, r as f64, radius) else {
            continue;
        };
        worst = worst.max((r as f64 - l as f64).abs() * 1000.0 / f64::from(OUT_RATE));
        pairs += 1;
    }
    (worst, pairs)
}

struct Case {
    out_bpm: f64,
    in_bpm: f64,
    in_rate: u32,
    in_first: f64,
    cap_ms: u64,
    /// Scatter put on both beat lists, in seconds: the tracker placing
    /// each beat a little early or late, while the clicks stay on time.
    scatter: f64,
}

/// A small deterministic scatter, -max..max seconds.
fn scattered(beats_s: &[f64], max: f64, seed: u32) -> Vec<f64> {
    let mut x = seed;
    beats_s
        .iter()
        .map(|&b| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (b + (f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0) * max).max(0.0)
        })
        .collect()
}

fn mix_case(case: &Case) -> (automix::Plan, f64, usize) {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("out.wav");
    let in_path = dir.path().join("in.wav");
    let out_clicks = clicks(case.out_bpm, 0.5, 40.0);
    let in_clicks = clicks(case.in_bpm, case.in_first, 30.0);
    click_track(&out_path, 48_000, 40.5, &out_clicks, 0);
    click_track(&in_path, case.in_rate, 30.0, &in_clicks, 1);
    let out_beats = stored(&scattered(&out_clicks, case.scatter, 0x9e37_79b9));
    let in_beats = stored(&scattered(&in_clicks, case.scatter, 0x85eb_ca6b));
    let plan = automix::plan(
        TrackBeats {
            beats: &out_beats,
            duration_ms: 40_500,
        },
        TrackBeats {
            beats: &in_beats,
            duration_ms: 30_000,
        },
        case.cap_ms,
    )
    .unwrap();
    let mixed = run(&out_path, 40_500, &in_path, 30_000, &plan);
    let (worst, pairs) = measure(&mixed, &in_clicks);
    (plan, worst, pairs)
}

/// Same tempo, both at the output rate: the clicks coincide to within a
/// millisecond over the whole fade.
#[test]
fn same_tempo_beats_coincide_in_the_mix() {
    let (plan, worst, pairs) = mix_case(&Case {
        out_bpm: 120.0,
        in_bpm: 120.0,
        in_rate: 48_000,
        in_first: 0.3,
        cap_ms: 8_000,
        scatter: 0.0,
    });
    assert!(pairs >= plan.beats as usize - 1, "{pairs} pairs, {plan:?}");
    assert!(worst <= 1.0, "worst {worst:.2} ms, {plan:?}");
}

/// An incoming track at 44.1 kHz goes through the resampler, whose delay
/// would put its beats late by that much without the correction.
#[test]
fn a_resampled_incoming_track_lands_on_the_beat_too() {
    let (plan, worst, pairs) = mix_case(&Case {
        out_bpm: 126.0,
        in_bpm: 126.0,
        in_rate: 44_100,
        in_first: 0.45,
        cap_ms: 8_000,
        scatter: 0.0,
    });
    assert!(pairs >= plan.beats as usize - 1, "{pairs} pairs, {plan:?}");
    assert!(worst <= 1.5, "worst {worst:.2} ms, {plan:?}");
}

/// Close tempos drift apart, but no further than the plan allows; the
/// measurement agrees with the prediction.
#[test]
fn a_close_tempo_drifts_no_further_than_planned() {
    let (plan, worst, pairs) = mix_case(&Case {
        out_bpm: 120.0,
        in_bpm: 118.5,
        in_rate: 44_100,
        in_first: 0.2,
        cap_ms: 10_000,
        scatter: 0.0,
    });
    assert!(pairs >= 3, "{pairs} pairs, {plan:?}");
    assert!(
        worst <= automix::MAX_DRIFT_MS + 1.5,
        "worst {worst:.2} ms, {plan:?}"
    );
    assert!(
        (worst - plan.max_drift_ms).abs() <= 2.0,
        "worst {worst:.2} ms, {plan:?}"
    );
}

/// Half tempo: the slower track's clicks land on every other beat.
#[test]
fn a_half_tempo_track_lands_on_every_other_beat() {
    let (plan, worst, pairs) = mix_case(&Case {
        out_bpm: 128.0,
        in_bpm: 64.0,
        in_rate: 48_000,
        in_first: 0.25,
        cap_ms: 8_000,
        scatter: 0.0,
    });
    assert_eq!(plan.fold, 2.0);
    assert!(pairs >= 2, "{pairs} pairs, {plan:?}");
    assert!(worst <= 1.0, "worst {worst:.2} ms, {plan:?}");
}

/// The tracker's scatter (±15 ms on every beat of both lists) is averaged
/// out by the grid fit: the clicks, which are on time, still land within
/// a few milliseconds of each other.
#[test]
fn tracker_scatter_on_the_beat_lists_barely_moves_the_clicks() {
    let (plan, worst, pairs) = mix_case(&Case {
        out_bpm: 124.0,
        in_bpm: 124.0,
        in_rate: 44_100,
        in_first: 0.35,
        cap_ms: 8_000,
        scatter: 0.015,
    });
    assert!(pairs >= plan.beats as usize - 1, "{pairs} pairs, {plan:?}");
    assert!(worst <= 6.0, "worst {worst:.2} ms, {plan:?}");
}
