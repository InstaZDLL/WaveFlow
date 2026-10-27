use super::*;

const FPS: f64 = 100.0;

/// An envelope with a spike every `period` frames from `start`.
fn pulses(len: usize, start: usize, period: f64) -> Vec<f32> {
    let mut out = vec![0.0f32; len];
    let mut k = 0;
    loop {
        let at = start + (k as f64 * period).round() as usize;
        if at >= len {
            break;
        }
        out[at] = 1.0;
        k += 1;
    }
    out
}

#[test]
fn beats_land_on_the_pulses() {
    // 120 BPM at 100 frames per second: a pulse every 50 frames.
    let onset = pulses(3000, 17, 50.0);
    let beats = track(&onset, FPS, 120.0);
    assert!(beats.len() >= 55, "{}", beats.len());
    for b in &beats {
        assert_eq!((b - 17) % 50, 0, "beat at {b}");
    }
}

#[test]
fn a_drifting_pulse_is_followed() {
    // The gaps widen from 48 to 52 frames: a band slowing down.
    let mut onset = vec![0.0f32; 3200];
    let mut at = 10.0;
    let mut truth = Vec::new();
    for k in 0..60 {
        truth.push(at as usize);
        onset[at as usize] = 1.0;
        at += 48.0 + 4.0 * k as f64 / 60.0;
    }
    let beats = track(&onset, FPS, 120.0);
    let hit = truth
        .iter()
        .filter(|&&t| beats.iter().any(|&b| b.abs_diff(t) <= 1))
        .count();
    assert!(hit >= 57, "{hit} of {}", truth.len());
}

#[test]
fn louder_offbeats_do_not_pull_the_chain_off_the_tempo() {
    // Beats every 50 frames, and a louder hit 25 frames after each:
    // at the tempo given, the chain keeps one of the two lines rather
    // than taking both.
    let mut onset = pulses(3000, 0, 50.0);
    for (i, v) in pulses(3000, 25, 50.0).into_iter().enumerate() {
        onset[i] += 1.5 * v;
    }
    let beats = track(&onset, FPS, 120.0);
    let gaps: Vec<usize> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.iter().all(|g| g.abs_diff(50) <= 1), "{gaps:?}");
}

#[test]
fn a_beat_list_survives_storage() {
    let beats: Vec<f64> = (0..500).map(|k| 0.1234 + k as f64 * 0.4999).collect();
    let bytes = encode(&beats).expect("encodes");
    assert_eq!(bytes.len(), 5 + 2 * 499);
    let back = decode(&bytes).expect("decodes");
    assert_eq!(back.len(), beats.len());
    for (a, b) in beats.iter().zip(&back) {
        assert!((a - b).abs() <= 0.000_501, "{a} vs {b}");
    }
    assert!(encode(&[]).is_none());
    assert!(decode(&[]).is_none());
    assert!(decode(&[2, 0, 0, 0, 0]).is_none());
    assert!(decode(&[FORMAT, 0, 0, 0, 0, 1]).is_none());
}

#[test]
fn silence_has_no_beats() {
    assert!(track(&vec![0.0; 3000], FPS, 120.0).is_empty());
    assert!(track(&[], FPS, 120.0).is_empty());
}

#[test]
fn a_silent_intro_gets_no_beats() {
    let mut onset = vec![0.0f32; 1000];
    onset.extend(pulses(2000, 0, 50.0));
    let beats = track(&onset, FPS, 120.0);
    assert!(beats[0] >= 1000, "first beat at {}", beats[0]);
}
