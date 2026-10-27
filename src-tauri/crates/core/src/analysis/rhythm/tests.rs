use super::*;

const RATE: f64 = 11_025.0;

/// A click track: a short decaying noise burst on every beat, with an
/// optional softer one halfway between.
fn clicks(bpm: f64, secs: f64, offbeat: f32) -> Vec<f32> {
    let n = (secs * RATE) as usize;
    let period = 60.0 / bpm * RATE;
    let mut out = vec![0.0f32; n];
    let mut seed = 0x2545_f491_u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let burst = (0.03 * RATE) as usize;
    let mut beat = 0.0;
    let mut k = 0;
    while (beat as usize) < n {
        for (j, gain) in [(0.0, 0.8f32), (0.5, offbeat)] {
            if gain <= 0.0 {
                continue;
            }
            let at = (beat + j * period) as usize;
            for i in 0..burst {
                if at + i < n {
                    let decay = (-(i as f32) / (burst as f32 / 5.0)).exp();
                    out[at + i] += gain * decay * noise();
                }
            }
        }
        k += 1;
        beat = k as f64 * period;
    }
    out
}

fn analyse(signal: &[f32]) -> Rhythm {
    let mut detector = OnsetDetector::new(RATE);
    for &s in signal {
        detector.push(s);
    }
    detector.finish()
}

fn tempo_of(signal: &[f32]) -> Tempo {
    analyse(signal).tempo.expect("a tempo")
}

#[test]
fn reads_common_tempos_within_a_bpm() {
    for bpm in [70.0, 90.0, 120.0, 128.0, 140.0, 174.0] {
        let t = tempo_of(&clicks(bpm, 40.0, 0.0));
        // A plain click track has no octave to prefer beyond the
        // prior; what must hold is the reading or its double/half.
        let folded = [t.bpm, t.bpm * 2.0, t.bpm / 2.0]
            .into_iter()
            .any(|b| (b - bpm).abs() < 1.0);
        assert!(folded, "{bpm} read as {}", t.bpm);
    }
}

#[test]
fn eighth_notes_do_not_halve_a_fast_tempo() {
    // 170 BPM with softer off-beats: the drum'n'bass case that
    // version 1 read as 85.
    let t = tempo_of(&clicks(170.0, 40.0, 0.4));
    assert!((t.bpm - 170.0).abs() < 2.0, "read {}", t.bpm);
}

#[test]
fn a_steady_click_is_trusted() {
    let t = tempo_of(&clicks(120.0, 40.0, 0.3));
    assert!(t.stability > 0.9, "{t:?}");
    assert!(t.confidence > 0.5, "{t:?}");
}

#[test]
fn noise_is_not_trusted() {
    let mut seed = 7u32;
    let noise: Vec<f32> = (0..(40.0 * RATE) as usize)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed as f32 / u32::MAX as f32) * 0.2 - 0.1
        })
        .collect();
    if let Some(t) = analyse(&noise).tempo {
        assert!(t.confidence < 0.3, "{t:?}");
    }
}

#[test]
fn silence_and_short_clips_have_no_tempo() {
    assert!(analyse(&vec![0.0; (40.0 * RATE) as usize]).tempo.is_none());
    assert!(analyse(&clicks(120.0, 3.0, 0.0)).tempo.is_none());
}

#[test]
fn a_tempo_change_lowers_stability() {
    let mut signal = clicks(90.0, 30.0, 0.0);
    signal.extend(clicks(133.0, 30.0, 0.0));
    let steady = tempo_of(&clicks(90.0, 60.0, 0.0));
    let changing = tempo_of(&signal);
    assert!(
        changing.stability < steady.stability,
        "{changing:?} vs {steady:?}"
    );
}

#[test]
fn beats_fall_on_the_clicks() {
    for bpm in [96.0, 120.0, 150.0] {
        let rhythm = analyse(&clicks(bpm, 40.0, 0.0));
        let period = 60.0 / bpm;
        assert!(rhythm.beats.len() as f64 > 0.9 * 40.0 / period, "{bpm}");
        for b in &rhythm.beats {
            let off = b / period - (b / period).round();
            assert!((off * period).abs() < 0.012, "{bpm}: beat at {b}");
        }
    }
}

#[test]
fn energy_ignores_the_level() {
    let loud = clicks(128.0, 30.0, 0.5);
    let quiet: Vec<f32> = loud.iter().map(|s| s * 0.1).collect();
    let a = analyse(&loud).energy.expect("energy");
    let b = analyse(&quiet).energy.expect("energy");
    assert!((a.value - b.value).abs() < 0.03, "{a:?} vs {b:?}");
}

#[test]
fn dense_attacks_are_more_energetic_than_sparse_ones() {
    let dense = analyse(&clicks(170.0, 30.0, 0.7)).energy.expect("energy");
    let sparse = analyse(&clicks(60.0, 30.0, 0.0)).energy.expect("energy");
    assert!(dense.value > sparse.value, "{dense:?} vs {sparse:?}");
}
