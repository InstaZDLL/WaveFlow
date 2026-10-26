use super::*;

const RATE: f64 = 11_025.0;

/// A sustained chord: the given MIDI notes as sines, with a few
/// harmonics each, for `secs` seconds.
fn chord(notes: &[u8], secs: f64) -> Vec<f32> {
    let n = (secs * RATE) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / RATE;
            notes
                .iter()
                .map(|&m| {
                    let f = 440.0 * 2f64.powf((f64::from(m) - 69.0) / 12.0);
                    (1..=3)
                        .map(|h| (2.0 * std::f64::consts::PI * f * h as f64 * t).sin() / h as f64)
                        .sum::<f64>()
                })
                .sum::<f64>() as f32
                * 0.1
        })
        .collect()
}

fn detect(signal: &[f32]) -> Option<Key> {
    let mut d = KeyDetector::new(RATE);
    for &s in signal {
        d.push(s);
    }
    d.finish().0
}

/// I–IV–V–I in a key, four seconds each: the cadence that defines it.
fn cadence(tonic: u8, minor: bool) -> Vec<f32> {
    let third = if minor { 3 } else { 4 };
    let triad = |root: u8, third: u8| vec![root, root + third, root + 7, root - 12];
    let mut out = Vec::new();
    for (root, t) in [
        (tonic, third),
        (tonic + 5, third),
        (tonic + 7, 4), // the dominant is major in minor too
        (tonic, third),
    ] {
        out.extend(chord(&triad(root, t), 4.0));
    }
    out
}

#[test]
fn reads_a_major_cadence() {
    let key = detect(&cadence(60, false)).expect("a key");
    assert_eq!(key.name(), "C", "{key:?}");
}

#[test]
fn reads_a_minor_cadence() {
    let key = detect(&cadence(57, true)).expect("a key");
    assert_eq!(key.name(), "Am", "{key:?}");
}

#[test]
fn reads_every_tonic_of_a_major_cadence() {
    for tonic in 0..12u8 {
        let key = detect(&cadence(60 + tonic, false)).expect("a key");
        assert_eq!(key.tonic, tonic, "{key:?}");
        assert!(!key.minor, "{key:?}");
    }
}

/// Broadband bursts on every beat over the cadence — the drums the
/// whitening exists for — must not move the key.
#[test]
fn drums_do_not_move_the_key() {
    let mut signal = cadence(57, true);
    let mut seed = 0x9e37_79b9_u32;
    let beat = (0.5 * RATE) as usize;
    for start in (0..signal.len()).step_by(beat) {
        for i in 0..(0.05 * RATE) as usize {
            if start + i < signal.len() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
                signal[start + i] += 0.6 * noise * (-(i as f32) / 200.0).exp();
            }
        }
    }
    let key = detect(&signal).expect("a key");
    assert_eq!(key.name(), "Am", "{key:?}");
}

#[test]
fn silence_and_short_clips_have_no_key() {
    assert!(detect(&vec![0.0; (20.0 * RATE) as usize]).is_none());
    assert!(detect(&chord(&[60, 64, 67], 2.0)).is_none());
}

#[test]
fn names_use_key_signature_spelling() {
    let k = |tonic, minor| {
        Key {
            tonic,
            minor,
            confidence: 1.0,
        }
        .name()
    };
    assert_eq!(k(3, false), "Eb");
    assert_eq!(k(6, true), "F#m");
    assert_eq!(k(8, true), "G#m");
    assert_eq!(k(10, false), "Bb");
}
