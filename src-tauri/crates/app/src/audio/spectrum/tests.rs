use super::*;

#[test]
fn the_delay_counts_the_ring_and_the_buffer_up_to_the_window_middle() {
    // 1 s of stereo queued at 48 kHz, a 1024-frame buffer fed, and the
    // window exactly full: its middle is FFT_SIZE / 2 frames before the
    // buffer's end.
    let ms = delay_ms(96_000, 2048, FFT_SIZE, 2, 48_000.0);
    let frames = 48_000 + 1024 - FFT_SIZE as u64 / 2;
    assert_eq!(u64::from(ms), (frames * 1000 + 24_000) / 48_000);
    // Nothing queued and the window's middle already past the buffer:
    // never negative.
    assert_eq!(delay_ms(0, 0, FFT_SIZE * 4, 2, 48_000.0), 0);
}

#[test]
fn hann_window_is_symmetric_and_unit_peak() {
    // Use an odd length so the centre index is an integer and the
    // window hits exactly 1.0: w[(n-1)/2] = 0.5*(1-cos(π)) = 1.0.
    // An even-length window (e.g. 8) has its theoretical peak
    // between two samples (~0.9505 for n=8), which would never
    // satisfy a 1e-3 tolerance.
    let w = hann_window(9);
    // First and last samples are zero, middle peaks at 1.0.
    assert!(w[0] < 1e-6);
    assert!(w[w.len() - 1] < 1e-6);
    let max = w.iter().copied().fold(f32::MIN, f32::max);
    assert!((max - 1.0).abs() < 1e-6);
}

#[test]
fn empty_spectrum_produces_zero_bands() {
    let mut bands = vec![1.0; BAND_COUNT];
    band_magnitudes(&[], 44_100.0, &mut bands);
    assert!(bands.iter().all(|&v| v == 0.0));
}

/// The #715 defect: adjacent bass bands reading the very same bin.
/// A sloped spectrum must give every band its own value.
#[test]
fn no_two_bass_bands_read_the_same_value() {
    let spectrum: Vec<Complex<f32>> = (0..=FFT_SIZE / 2)
        .map(|i| Complex::new(1000.0 / (1.0 + i as f32), 0.0))
        .collect();
    let mut bands = vec![0.0; BAND_COUNT];
    band_magnitudes(&spectrum, 44_100.0, &mut bands);
    for pair in bands[..12].windows(2) {
        assert!(
            (pair[0] - pair[1]).abs() > 1e-3,
            "adjacent bass bands must differ: {pair:?}"
        );
    }
}

#[test]
fn a_quiet_master_still_reaches_the_top() {
    // A frame peak far below the old fixed reference of 250.
    let mut level = MIN_LEVEL;
    for _ in 0..60 {
        level = follow_level(level, 60.0, EMIT_INTERVAL);
    }
    let mut bands = [0.0; 1];
    scale_bands(&[60.0], level, &mut bands);
    assert!(bands[0] > 0.9, "got {}", bands[0]);
}

#[test]
fn silence_is_not_amplified_into_noise() {
    let mut level = 400.0;
    for _ in 0..600 {
        level = follow_level(level, 0.5, EMIT_INTERVAL);
    }
    assert!(level >= MIN_LEVEL);
    let mut bands = [1.0; 1];
    scale_bands(&[0.5], level, &mut bands);
    assert_eq!(bands[0], 0.0);
}

#[test]
fn a_single_beat_does_not_reset_the_scale() {
    // Rising takes a fraction of a second: one loud frame moves the
    // reference only part of the way, so the beat itself reaches the
    // top of the scale.
    let level = follow_level(100.0, 1000.0, EMIT_INTERVAL);
    assert!(level < 300.0, "got {level}");
}

/// A hit several times louder than the reference used to clip every
/// band of it to 1 — a flat plateau at the top of the display. The
/// louder band now still stands above the other, and neither reaches
/// the top.
#[test]
fn bands_louder_than_the_reference_stay_apart() {
    let mut bands = [0.0; 2];
    scale_bands(&[200.0, 400.0], MIN_LEVEL, &mut bands);
    assert!(bands[0] < bands[1], "got {bands:?}");
    assert!(bands[1] < 1.0, "got {bands:?}");
    assert!(bands[0] > 0.9, "got {bands:?}");
}

#[test]
fn the_soft_limit_is_continuous_at_the_knee() {
    assert_eq!(soft_limit(KNEE), KNEE);
    assert!((soft_limit(KNEE + 1e-3) - (KNEE + 1e-3)).abs() < 1e-5);
    assert_eq!(soft_limit(0.3), 0.3);
}

#[test]
fn analyzer_starts_idle_and_resets_clean() {
    let mut a = SpectrumAnalyzer::new();
    // Pretend we accumulated a few samples then reset — pending
    // must be cleared so a freshly loaded track doesn't inherit
    // stale data.
    a.pending.extend(std::iter::repeat(0.5_f32).take(100));
    a.reset();
    assert!(a.pending.is_empty());
}
