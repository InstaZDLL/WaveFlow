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

/// A stereo track on a 7.1 output is two channels and six silent ones, and
/// reads as the two.
#[test]
fn only_the_channels_carrying_something_count_in_the_mono_mix() {
    let stereo_on_eight = [0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    assert_eq!(channels_in_use(&stereo_on_eight, 8), 2);
    let surround = [0.5, -0.5, 0.1, 0.0, 0.2, 0.2, 0.3, 0.1];
    assert_eq!(channels_in_use(&surround, 8), 7);
    assert_eq!(channels_in_use(&[0.0; 16], 8), 1);
    assert_eq!(channels_in_use(&[], 8), 1);
}

/// A verse after the chorus: the reference must still remember the
/// chorus ten seconds in, so the verse is drawn lower than the chorus
/// was. With the old three-second release it was back at the floor by
/// then, and the verse filled the display as the chorus had.
#[test]
fn a_quiet_passage_stays_quiet() {
    let mut level = MIN_LEVEL;
    for _ in 0..90 {
        level = follow_level(level, 230.0, EMIT_INTERVAL);
    }
    let mut chorus = [0.0; 1];
    scale_bands(&[230.0], level, &mut chorus);
    for _ in 0..300 {
        level = follow_level(level, 46.0, EMIT_INTERVAL);
    }
    let mut verse = [0.0; 1];
    scale_bands(&[46.0], level, &mut verse);
    assert!(verse[0] < 0.7, "got {}", verse[0]);
    assert!(verse[0] < chorus[0] - 0.25, "{verse:?} vs {chorus:?}");
}

/// A band that alternates between two values, frame after frame, is
/// how the estimate of a steady noise looks: it settles between them
/// rather than swinging the full distance on every frame.
#[test]
fn smoothing_steadies_a_band_that_flickers() {
    let mut power = [0.0; 1];
    let mut swing = 0.0f32;
    let mut last = 0.0f32;
    for i in 0..60 {
        let mut band = [if i % 2 == 0 { 100.0 } else { 50.0 }];
        smooth_bands(&mut power, &mut band, EMIT_INTERVAL, i == 0);
        if i >= 30 {
            swing = swing.max((band[0] - last).abs());
        }
        last = band[0];
    }
    assert!(swing < 25.0, "swing {swing}");
}

/// A sustained hit is not held back: within a few frames it is at its
/// own height, and the first frame after a reset takes the band as is.
#[test]
fn smoothing_lets_a_sustained_hit_through() {
    let mut power = [0.0; 1];
    let mut band = [100.0];
    smooth_bands(&mut power, &mut band, EMIT_INTERVAL, true);
    assert_eq!(band[0], 100.0);

    let mut power = [0.0; 1];
    let mut band = [0.0];
    for _ in 0..5 {
        band = [100.0];
        smooth_bands(&mut power, &mut band, EMIT_INTERVAL, false);
    }
    assert!(band[0] > 95.0, "got {}", band[0]);
}

/// End to end on the analyzer: steady white noise, a fresh window every
/// frame. Each band's estimate wanders on its own from one window to the
/// next; the frames that reach the display must not.
#[test]
fn steady_noise_does_not_flicker() {
    let mut a = SpectrumAnalyzer::new();
    let mut seed = 0x2545_f491_u32;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.25
    };
    let mut previous = vec![0.0; BAND_COUNT];
    let mut moved = 0.0f32;
    let mut counted = 0;
    for frame in 0..120 {
        a.pending.clear();
        a.pending.extend((0..FFT_SIZE).map(|_| noise()));
        a.run_one_frame(44_100.0, EMIT_INTERVAL, frame == 0);
        if frame >= 30 {
            moved += a
                .bands
                .iter()
                .zip(&previous)
                .map(|(b, p)| (b - p).abs())
                .sum::<f32>();
            counted += BAND_COUNT;
        }
        previous.copy_from_slice(&a.bands);
    }
    let mean_move = moved / counted as f32;
    assert!(mean_move < 0.04, "bands moved {mean_move} per frame");
}
