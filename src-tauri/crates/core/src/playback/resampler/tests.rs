use super::*;

/// The end of a track comes out whole, and stops at the track's
/// length: what the resampler held is not lost, and the zeros that
/// pushed it out do not become a gap before the next track.
#[test]
fn finish_brings_the_output_to_the_track_length() {
    let channels = 2;
    let (src, dst) = (48_000_u32, 44_100_u32);
    let mut r = Resampler::new(src, dst, channels).expect("ctor");
    let delay = match &r {
        Resampler::Fft { inner, .. } => inner.output_delay(),
        Resampler::Passthrough => unreachable!(),
    };
    let mut out = Vec::new();
    // One second of a steady level, in pieces that are not chunks.
    for _ in 0..48 {
        r.process(&vec![0.5_f32; 1_000 * channels], &mut out)
            .expect("process");
    }
    let expected = delay + 44_100;
    assert!(out.len() / channels < expected, "the tail is still inside");

    r.finish(&mut out).expect("finish");

    assert_eq!(out.len() / channels, expected);
    // Well inside the signal's end, the level is still there.
    let frame = (expected - 300) * channels;
    assert!((out[frame] - 0.5).abs() < 0.05, "got {}", out[frame]);
}

/// At an odd rate rubato's FFT spans many of our chunks: a track a
/// chunk long (44.1 kHz at 1.125x is 49 612 Hz, to 48 kHz) must still
/// come out at its length rather than not at all.
#[test]
fn finish_reaches_the_length_when_the_fft_spans_many_chunks() {
    let channels = 2;
    let (src, dst) = (49_612_u32, 48_000_u32);
    let mut r = Resampler::new(src, dst, channels).expect("ctor");
    let delay = match &r {
        Resampler::Fft { inner, .. } => inner.output_delay(),
        Resampler::Passthrough => unreachable!(),
    };
    let mut out = Vec::new();
    r.process(&vec![0.5_f32; 1_024 * channels], &mut out)
        .expect("process");

    r.finish(&mut out).expect("finish");

    let length = (1_024_u64 * u64::from(dst) + u64::from(src) / 2) / u64::from(src);
    assert_eq!(out.len() / channels, delay + length as usize);
}

/// The same, over the rates playback can ask for: three source
/// rates, speeds from 0.5x to 2x, two outputs, a track one chunk long
/// and one a second long.
#[test]
fn finish_reaches_the_length_at_every_playback_rate() {
    let channels = 2;
    for src in [44_100_u32, 48_000, 96_000] {
        for speed in [0.5_f32, 0.75, 0.9, 1.125, 1.25, 1.5, 2.0] {
            let effective = ((src as f32) * speed) as u32;
            for dst in [44_100_u32, 48_000] {
                for frames in [1_024_usize, src as usize] {
                    let mut r = Resampler::new(effective, dst, channels).expect("ctor");
                    let delay = match &r {
                        Resampler::Fft { inner, .. } => inner.output_delay(),
                        Resampler::Passthrough => continue,
                    };
                    let mut out = Vec::new();
                    r.process(&vec![0.25_f32; frames * channels], &mut out)
                        .expect("process");
                    r.finish(&mut out).expect("finish");
                    let length = (frames as u64 * u64::from(dst) + u64::from(effective) / 2)
                        / u64::from(effective);
                    assert_eq!(
                        out.len() / channels,
                        delay + length as usize,
                        "{effective} Hz to {dst} Hz, {frames} frames"
                    );
                }
            }
        }
    }
}

/// After a reset nothing of the old position comes out: a level fed
/// before it is gone from what follows.
#[test]
fn reset_forgets_the_old_position() {
    let channels = 2;
    let mut r = Resampler::new(48_000, 44_100, channels).expect("ctor");
    let mut out = Vec::new();
    r.process(&vec![0.5_f32; 4_000 * channels], &mut out)
        .expect("process");

    r.reset();
    out.clear();
    r.process(&vec![0.0_f32; 4_000 * channels], &mut out)
        .expect("process");
    r.finish(&mut out).expect("finish");

    let loudest = out.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(loudest < 1e-3, "the old level leaked through: {loudest}");
}

#[test]
fn finish_with_nothing_given_adds_nothing() {
    let mut r = Resampler::new(48_000, 44_100, 2).expect("ctor");
    let mut out = Vec::new();
    r.finish(&mut out).expect("finish");
    assert!(out.is_empty());
}

#[test]
fn finish_on_passthrough_adds_nothing() {
    let mut r = Resampler::new(44_100, 44_100, 2).expect("ctor");
    let mut out = vec![0.1_f32, 0.2];
    r.finish(&mut out).expect("finish");
    assert_eq!(out, [0.1, 0.2]);
}

#[test]
fn passthrough_returns_input_unchanged() {
    let mut r = Resampler::new(48_000, 48_000, 2).expect("ctor");
    let input = vec![0.1_f32, 0.2, 0.3, 0.4, 0.5, 0.6];
    let mut out = Vec::new();
    r.process(&input, &mut out).expect("process");
    assert_eq!(out, input);
}

#[test]
fn fft_48k_to_44_1k_produces_proportional_output() {
    let channels = 2;
    let src = 48_000_u32;
    let dst = 44_100_u32;
    let mut r = Resampler::new(src, dst, channels).expect("ctor");

    // Feed ~1s of stereo silence in chunks of 4096 frames.
    let total_frames = src as usize;
    let mut out = Vec::new();
    let chunk_frames = 4096;
    let mut fed = 0;
    while fed < total_frames {
        let take = chunk_frames.min(total_frames - fed);
        let buf = vec![0.0_f32; take * channels];
        r.process(&buf, &mut out).expect("process");
        fed += take;
    }

    // Output should be roughly `total_frames * dst / src` interleaved
    // frames. Tolerance covers the trailing partial rubato chunk we
    // haven't drained yet.
    let expected_frames = total_frames * dst as usize / src as usize;
    let produced_frames = out.len() / channels;
    let diff = (produced_frames as i64 - expected_frames as i64).abs();
    assert!(
        diff < 2_048,
        "expected ~{expected_frames} frames, got {produced_frames} (diff {diff})"
    );
    assert_eq!(out.len() % channels, 0, "output not aligned to channels");
}
