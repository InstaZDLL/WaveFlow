use super::*;

const RATE: f32 = 44_100.0;

fn chain(gains: [f32; BAND_COUNT]) -> [[f32; 5]; BAND_COUNT] {
    let mut coeffs = [[1.0, 0.0, 0.0, 0.0, 0.0]; BAND_COUNT];
    for (i, &freq) in BAND_FREQS.iter().enumerate() {
        coeffs[i] = peaking_biquad(freq, RATE, gains[i], BAND_Q);
    }
    coeffs
}

fn db(linear: f32) -> f32 {
    20.0 * linear.log10()
}

/// A new preamp is reached over the buffer, not between two samples.
#[test]
fn the_preamp_ramps_across_a_buffer() {
    let mut buf = vec![1.0_f32; 8]; // 4 stereo frames
    apply_preamp(&mut buf, 2, 1.0, 0.5);
    assert_eq!(buf, vec![0.875, 0.875, 0.75, 0.75, 0.625, 0.625, 0.5, 0.5]);

    let mut steady = vec![0.3_f32, -0.7, 0.1, 0.9];
    apply_preamp(&mut steady, 2, 1.0, 1.0);
    assert_eq!(steady, vec![0.3, -0.7, 0.1, 0.9]);
}

/// More attenuation is applied at once, less is given back over the
/// next buffer.
#[test]
fn the_preamp_drops_at_once_and_recovers_over_a_buffer() {
    let shared = EqShared::new();
    shared.set_enabled(true);
    shared.set_band_db(3, 6.0);
    let mut eq = EqProcessor::new();
    let mut silence = vec![0.0_f32; 64];
    eq.process(&mut silence, 1, RATE, &shared);
    let boosted = eq.preamp;
    assert!(boosted < 0.5, "{boosted}");

    shared.set_band_db(3, 0.0);
    let mut buf = vec![1.0_f32; 4];
    eq.process(&mut buf, 1, RATE, &shared);
    // Flat again: the gain climbs back to 1.0 over the buffer, from
    // where it was, rather than jumping.
    assert!(buf[0] < 0.9 && buf[0] > boosted, "{buf:?}");
    assert!((buf[3] - 1.0).abs() < 1e-6, "{buf:?}");
    assert_eq!(eq.preamp, 1.0);
}

#[test]
fn a_flat_or_cutting_setting_leaves_the_level_alone() {
    assert_eq!(headroom_preamp(&chain([0.0; BAND_COUNT]), RATE), 1.0);
    let cuts = preset_gains("bass_reducer").unwrap();
    assert_eq!(headroom_preamp(&chain(cuts), RATE), 1.0);
}

#[test]
fn a_lone_boost_is_matched_by_the_preamp() {
    let mut gains = [0.0; BAND_COUNT];
    gains[3] = 6.0;
    // The boost, plus the 1 dB transient margin.
    let preamp = db(headroom_preamp(&chain(gains), RATE));
    assert!((preamp + 7.0).abs() < 0.1, "{preamp} dB");
}

#[test]
fn overlapping_boosts_take_more_than_the_largest_slider() {
    // Bass booster: +6 / +5 / +3 dB on the three lowest bands, which
    // overlap; their sum peaks past the +6 dB slider.
    let gains = preset_gains("bass_booster").unwrap();
    let preamp = db(headroom_preamp(&chain(gains), RATE));
    assert!(preamp < -7.5, "{preamp} dB");
}

/// The worst transient measured: a sharp full-scale pattern through
/// +12 dB at 15 kHz overshoots the response peak by 0.9 dB, which the
/// margin absorbs.
#[test]
fn a_sharp_full_scale_pattern_under_a_treble_boost_stays_under_unity() {
    let shared = EqShared::new();
    shared.set_enabled(true);
    shared.set_band_db(BAND_COUNT - 1, MAX_GAIN_DB);
    let mut eq = EqProcessor::new();
    let mut buf: Vec<f32> = [1.0, -1.0, -1.0, 1.0, -1.0, -1.0, 1.0].repeat(3);
    eq.process(&mut buf, 1, RATE, &shared);
    let peak = buf.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
    assert!(peak <= 1.0, "peak {peak}");
}

#[test]
fn a_full_scale_tone_under_full_boost_stays_under_unity() {
    let shared = EqShared::new();
    shared.set_enabled(true);
    shared.set_band_db(0, MAX_GAIN_DB);
    let mut eq = EqProcessor::new();
    // One second of a full-scale 60 Hz sine, mono.
    let mut buf: Vec<f32> = (0..RATE as usize)
        .map(|n| (2.0 * PI * 60.0 * n as f32 / RATE).sin())
        .collect();
    eq.process(&mut buf, 1, RATE, &shared);
    // Past the filter's settling time, the output peaks just under
    // full scale (the 1 dB margin) instead of +12 dB over it.
    let settled = &buf[buf.len() / 2..];
    let peak = settled.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
    assert!(peak <= 0.9 && peak > 0.85, "peak {peak}");
}
