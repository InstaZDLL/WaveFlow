use super::*;

/// Interleaved sine at `freq` Hz whose **peak** amplitude is
/// `peak_dbfs`, on every channel.
fn sine(freq: f64, peak_dbfs: f64, rate: u32, channels: usize, seconds: f64) -> Vec<f32> {
    let amplitude = 10f64.powf(peak_dbfs / 20.0);
    let frames = (f64::from(rate) * seconds) as usize;
    let mut out = Vec::with_capacity(frames * channels);
    for n in 0..frames {
        let t = n as f64 / f64::from(rate);
        let s = (amplitude * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
        for _ in 0..channels {
            out.push(s);
        }
    }
    out
}

fn measure(samples: &[f32], rate: u32, channels: usize) -> Option<f64> {
    let mut meter = LoudnessMeter::new(rate, channels);
    meter.push_interleaved(samples);
    meter.finish()
}

/// The coefficients re-derived at 48 kHz must reproduce the table
/// printed in BS.1770-4 — that table is the only external anchor
/// this file has, so if the derivation drifts, everything else
/// here is confidently wrong.
#[test]
fn derived_coefficients_match_the_published_48khz_table() {
    let shelf = Biquad::k_shelf(48_000.0);
    for (got, want) in [
        (shelf.b0, 1.535_124_859_586_97e0),
        (shelf.b1, -2.691_696_189_406_38e0),
        (shelf.b2, 1.198_392_810_852_85e0),
        (shelf.a1, -1.690_659_293_182_41e0),
        (shelf.a2, 0.732_480_774_215_85e0),
    ] {
        assert!(
            (got - want).abs() < 1e-5,
            "shelf coefficient drifted: {got} vs {want}"
        );
    }

    let hpf = Biquad::rlb_highpass(48_000.0);
    assert!((hpf.a1 - -1.990_047_454_833_98).abs() < 1e-5, "rlb a1");
    assert!((hpf.a2 - 0.990_072_250_366_21).abs() < 1e-5, "rlb a2");
}

/// BS.1770-4 weights the surround channels at G = 1.41, so the
/// same signal on a surround pair reads +1.5 dB against a front
/// pair. Weighting them equally — what this used to do — reads a
/// 5.1 mix quiet and hands out too much gain.
#[test]
fn surround_channels_carry_more_weight_than_front_ones() {
    let samples = sine(1000.0, -20.0, 48_000, 2, 3.0);

    let mut front = LoudnessMeter::with_channel_weights(48_000, &[1.0, 1.0]);
    front.push_interleaved(&samples);
    let front = front.finish().unwrap();

    let mut surround = LoudnessMeter::with_channel_weights(48_000, &[1.41, 1.41]);
    surround.push_interleaved(&samples);
    let surround = surround.finish().unwrap();

    assert!(
        (surround - front - 1.5).abs() < 0.05,
        "surround read {surround} LUFS against {front} for the same signal"
    );
}

/// LFE is excluded outright (weight 0.0), not merely attenuated.
/// A loud LFE track would otherwise measure far louder than it
/// sounds and be handed too small a gain — and a stream that is
/// nothing but LFE has no measurable loudness at all.
#[test]
fn the_lfe_channel_is_excluded_from_the_measurement() {
    let samples = sine(1000.0, -20.0, 48_000, 2, 3.0);

    // Channel 1 is LFE: the result must match the other channel
    // measured alone.
    let mut with_lfe = LoudnessMeter::with_channel_weights(48_000, &[1.0, 0.0]);
    with_lfe.push_interleaved(&samples);
    let with_lfe = with_lfe.finish().unwrap();

    let mono: Vec<f32> = samples.iter().step_by(2).copied().collect();
    let mut alone = LoudnessMeter::with_channel_weights(48_000, &[1.0]);
    alone.push_interleaved(&mono);
    let alone = alone.finish().unwrap();

    assert!(
        (with_lfe - alone).abs() < 1e-6,
        "the LFE channel leaked into the measurement: {with_lfe} vs {alone}"
    );

    // An LFE-only stream measures nothing at all.
    let mut lfe_only = LoudnessMeter::with_channel_weights(48_000, &[0.0]);
    lfe_only.push_interleaved(&mono);
    assert_eq!(lfe_only.finish(), None);
}

/// EBU Tech 3341 case 1: a 1 kHz stereo sine reads its own level.
/// This is what the `−0.691` calibration constant exists for.
#[test]
fn a_1khz_sine_reads_its_own_level() {
    for level in [-23.0, -10.0, -40.0] {
        let lufs = measure(&sine(1000.0, level, 48_000, 2, 3.0), 48_000, 2)
            .expect("a 3 s sine produces gating blocks");
        assert!(
            (lufs - level).abs() < 0.15,
            "1 kHz at {level} dBFS read {lufs} LUFS"
        );
    }
}

/// The same signal must measure the same at any sample rate, or
/// a library ripped at mixed rates would get inconsistent gains.
#[test]
fn the_measurement_does_not_depend_on_the_sample_rate() {
    let reference = measure(&sine(1000.0, -23.0, 48_000, 2, 3.0), 48_000, 2).unwrap();
    for rate in [44_100, 88_200, 96_000] {
        let lufs = measure(&sine(1000.0, -23.0, rate, 2, 3.0), rate, 2).unwrap();
        assert!(
            (lufs - reference).abs() < 0.1,
            "{rate} Hz read {lufs} LUFS against {reference} at 48 kHz"
        );
    }
}

/// K-weighting is a *weighting*, which is the whole reason for
/// the rewrite: at equal amplitude, treble counts for more than
/// midrange and deep bass for much less. A flat RMS reads all
/// three the same. Both checks are points off the published
/// curve — the shelf's ~+4 dB plateau, and the RLB high-pass an
/// octave below its corner — not numbers read back off this
/// implementation.
#[test]
fn the_k_weighting_curve_shapes_the_measurement() {
    let reference = measure(&sine(1000.0, -20.0, 48_000, 2, 3.0), 48_000, 2).unwrap();

    let treble = measure(&sine(10_000.0, -20.0, 48_000, 2, 3.0), 48_000, 2).unwrap();
    assert!(
        (treble - reference - 4.0).abs() < 1.0,
        "10 kHz read {treble} LUFS against {reference} at 1 kHz — the shelf should add ~4 dB"
    );

    let bass = measure(&sine(20.0, -20.0, 48_000, 2, 3.0), 48_000, 2).unwrap();
    assert!(
        bass < reference - 8.0,
        "20 Hz read {bass} LUFS against {reference} at 1 kHz — the RLB should cut it hard"
    );
}

/// A fade-out or a long lead-in must not drag the number down:
/// that's the relative gate's job. Half a loud track plus half a
/// near-silent one should read close to the loud half alone.
#[test]
fn the_relative_gate_ignores_quiet_passages() {
    let loud = sine(1000.0, -20.0, 48_000, 2, 3.0);
    let quiet = sine(1000.0, -50.0, 48_000, 2, 3.0);

    let loud_only = measure(&loud, 48_000, 2).unwrap();
    let mixed = measure(&[loud, quiet].concat(), 48_000, 2).unwrap();

    assert!(
        (mixed - loud_only).abs() < 0.5,
        "gated {mixed} LUFS against {loud_only} for the loud half alone"
    );
}

/// Digital silence yields no gain suggestion at all rather than a
/// hugely positive one — a caller that boosted by
/// `target − (−∞)` would blow the speakers on the next track.
#[test]
fn silence_has_no_measurable_loudness() {
    assert_eq!(measure(&vec![0.0f32; 48_000 * 2 * 3], 48_000, 2), None);
}

/// Under one gating block there is nothing to report.
#[test]
fn a_file_shorter_than_one_block_is_not_measurable() {
    assert_eq!(
        measure(&sine(1000.0, -20.0, 48_000, 2, 0.2), 48_000, 2),
        None
    );
}
