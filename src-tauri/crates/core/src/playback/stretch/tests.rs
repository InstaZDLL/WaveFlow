use super::*;

const RATE: u32 = 44_100;

fn tone(freq: f64, seconds: f64, channels: usize, amplitude: f32) -> Vec<f32> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let phase = 2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(RATE);
        let s = phase.sin() as f32 * amplitude;
        out.resize(out.len() + channels, s);
    }
    out
}

fn stretch(input: &[f32], channels: usize, tempo: f64, chunk: usize) -> Vec<f32> {
    let mut s = Stretcher::new(RATE, channels, tempo).unwrap();
    let mut out = Vec::new();
    for c in input.chunks(chunk * channels) {
        s.process(c, &mut out).unwrap();
    }
    s.finish(&mut out).unwrap();
    out
}

/// Frequency of the strongest partial of channel 0, from zero crossings over
/// the steady middle of the signal.
fn measured_freq(signal: &[f32], channels: usize) -> f64 {
    let mono: Vec<f32> = signal.iter().step_by(channels).copied().collect();
    let part = &mono[mono.len() / 4..mono.len() * 3 / 4];
    let rising: Vec<f64> = part
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + f64::from(-w[0] / (w[1] - w[0])))
        .collect();
    let spans = (rising.len() - 1) as f64;
    spans * f64::from(RATE) / (rising[rising.len() - 1] - rising[0])
}

/// The DLLs a PE32+ image imports, from its import table. A search for the name
/// in the file will not do: it is there as data whether imported or not.
#[cfg(all(windows, target_env = "msvc"))]
fn imported_dlls(image: &[u8]) -> Vec<String> {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([image[at], image[at + 1]]));
    let u32_at = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap()) as usize;
    let pe = u32_at(0x3c);
    let (sections, optional_len) = (u16_at(pe + 6), u16_at(pe + 20));
    let optional = pe + 24;
    let section_table = optional + optional_len;
    let offset_of = |rva: usize| {
        (0..sections)
            .map(|i| section_table + i * 40)
            .find_map(|at| {
                let (start, size) = (u32_at(at + 12), u32_at(at + 16));
                (start..start + size)
                    .contains(&rva)
                    .then(|| rva - start + u32_at(at + 20))
            })
            .expect("an address outside every section")
    };
    // The second data directory, after the PE32+ optional header's 112 bytes.
    let mut descriptor = offset_of(u32_at(optional + 112 + 8));
    let mut names = Vec::new();
    while u32_at(descriptor + 12) != 0 {
        let name = &image[offset_of(u32_at(descriptor + 12))..];
        let end = name.iter().position(|b| *b == 0).unwrap();
        names.push(String::from_utf8_lossy(&name[..end]).to_ascii_lowercase());
        descriptor += 20;
    }
    names
}

/// The Windows app needs no redistributable: its C runtime is static, and the
/// C++ here must not bring in the DLL it would then need to start at all.
#[cfg(all(windows, target_env = "msvc"))]
#[test]
fn the_cpp_runtime_is_not_imported() {
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let dlls = imported_dlls(&exe);
    assert!(dlls.iter().any(|d| d == "kernel32.dll"), "{dlls:?}");
    assert!(!dlls.iter().any(|d| d.starts_with("msvcp")), "{dlls:?}");
    // And the stretcher it would be for is in this binary.
    assert!(Stretcher::new(RATE, 2, 1.25).is_ok());
}

/// Decided by the shift alone: at 1× with no transposition nothing stretches,
/// tape or not, and that is what keeps a bit-perfect output bit-perfect.
#[test]
fn only_a_real_shift_stretches() {
    let kept = |speed, semitones| TimeShift {
        speed,
        keep_pitch: true,
        semitones,
    };
    let tape = |speed, semitones| TimeShift {
        speed,
        keep_pitch: false,
        semitones,
    };
    assert!(!kept(1.0, 0.0).stretches());
    assert!(!tape(1.0, 0.0).stretches());
    assert!(kept(1.25, 0.0).stretches());
    assert!(kept(1.0, 1.0).stretches());
    assert!(!tape(1.25, 0.0).stretches());
    assert!(tape(1.25, 2.0).stretches());
    assert_eq!(kept(1.0, 0.0).resampler_rate(44_100), 44_100);
    assert_eq!(kept(1.25, 0.0).resampler_rate(44_100), 44_100);
    assert_eq!(tape(1.25, 0.0).resampler_rate(44_100), 55_125);
}

/// A rebuild drains the ring, so it comes only when the audio changes.
#[test]
fn only_a_change_of_the_audio_counts() {
    let shift = |speed, keep_pitch, semitones| TimeShift {
        speed,
        keep_pitch,
        semitones,
    };
    let plain = shift(1.0, true, 0.0);
    assert!(plain.same_audio(&shift(1.0, false, 0.0)));
    assert!(!plain.same_audio(&shift(1.25, true, 0.0)));
    assert!(!plain.same_audio(&shift(1.25, false, 0.0)));
    assert!(!plain.same_audio(&shift(1.0, true, 2.0)));
    assert!(!shift(1.25, true, 0.0).same_audio(&shift(1.25, false, 0.0)));
    assert!(!shift(1.25, true, 0.0).same_audio(&shift(1.5, true, 0.0)));
    assert!(!shift(1.0, true, 2.0).same_audio(&shift(1.0, true, 3.0)));
    assert!(shift(1.25, true, 2.0).same_audio(&shift(1.25, true, 2.0)));
}

/// The resampler's part and the stretcher's always make up the speed asked for,
/// or the clock would drift from the audio.
#[test]
fn the_two_stages_make_up_the_speed() {
    for keep_pitch in [true, false] {
        for speed in [0.5_f32, 0.75, 1.0, 1.1, 1.5, 2.0] {
            for semitones in [-12.0_f32, -5.0, -1.0, 0.0, 1.0, 7.0, 12.0] {
                let shift = TimeShift {
                    speed,
                    keep_pitch,
                    semitones,
                };
                if !shift.stretches() {
                    continue;
                }
                for src in [44_100_u32, 48_000, 96_000] {
                    let total = f64::from(shift.resampler_rate(src)) / f64::from(src)
                        * shift.stretch_tempo(src);
                    assert!(
                        (total - f64::from(speed)).abs() < 1e-9,
                        "{shift:?} at {src}: {total}"
                    );
                }
            }
        }
    }
}

/// A semitone's rate lands on a multiple rubato can work with, and close enough
/// not to be heard.
#[test]
fn a_transposed_rate_stays_in_tune() {
    let shift = |semitones| TimeShift {
        speed: 1.0,
        keep_pitch: true,
        semitones,
    };
    for src in [44_100_u32, 48_000, 96_000] {
        for semitones in -12..=12 {
            let semitones = semitones as f32;
            let rate = shift(semitones).resampler_rate(src);
            let cents =
                1200.0 * (f64::from(rate) / f64::from(src)).log2() - f64::from(semitones) * 100.0;
            assert!(cents.abs() < 0.5, "{semitones} at {src}: {cents} cents");
            if semitones != 0.0 {
                assert_eq!(rate % 10, 0);
            }
        }
    }
}

/// To the frame, whatever the speed and however the packets are cut: the clock
/// counts on it, and the next track of a gapless pair starts right after.
#[test]
fn the_length_is_the_input_at_the_tempo() {
    let input = tone(440.0, 3.0, 2, 0.3);
    let frames = input.len() / 2;
    for speed in [0.5_f32, 0.75, 1.1, 1.25, 1.5, 2.0] {
        for chunk in [1, 37, 1152, 8192] {
            if chunk == 1 && speed != 1.25 {
                continue;
            }
            let out = stretch(&input, 2, f64::from(speed), chunk);
            let expected = (frames as f64 / f64::from(speed)).round() as usize;
            assert_eq!(out.len() / 2, expected, "speed {speed}, chunk {chunk}");
        }
    }
}

/// A sound comes out where the speed says it should: no delay at the start and
/// none added along the way, or the position on screen would lag the audio.
#[test]
fn a_click_lands_at_its_scaled_time() {
    let frames = RATE as usize * 4;
    let mut input = vec![0.0_f32; frames * 2];
    let click_at = [RATE as usize / 2, RATE as usize * 2, RATE as usize * 3];
    for at in click_at {
        for i in 0..64 {
            let s = 0.8 * (1.0 - i as f32 / 64.0) * if i % 2 == 0 { 1.0 } else { -1.0 };
            input[(at + i) * 2] = s;
            input[(at + i) * 2 + 1] = s;
        }
    }
    for speed in [0.75_f32, 1.25, 2.0] {
        let out = stretch(&input, 2, f64::from(speed), 1152);
        for at in click_at {
            let expected = (at as f64 / f64::from(speed)) as usize;
            let window = RATE as usize / 10;
            let from = expected.saturating_sub(window);
            let to = (expected + window).min(out.len() / 2);
            let peak_at = (from..to)
                .max_by(|a, b| out[a * 2].abs().total_cmp(&out[b * 2].abs()))
                .unwrap();
            let error_ms = (peak_at as f64 - expected as f64).abs() * 1000.0 / f64::from(RATE);
            assert!(
                error_ms < 5.0,
                "speed {speed}, click at {at}: {error_ms} ms"
            );
        }
    }
}

#[test]
fn the_pitch_stays_when_the_speed_changes() {
    for speed in [0.5_f32, 0.75, 1.25, 1.5, 2.0] {
        let out = stretch(&tone(440.0, 4.0, 2, 0.3), 2, f64::from(speed), 1152);
        let cents = 1200.0 * (measured_freq(&out, 2) / 440.0).log2();
        assert!(cents.abs() < 2.0, "speed {speed}: {cents} cents");
    }
}

/// Eight channels stay eight channels with what each one carried: a tone on one
/// speaker does not leak to the others.
#[test]
fn channels_keep_their_own_signal() {
    let channels = 8;
    let source = tone(440.0, 3.0, 1, 0.5);
    let mut input = vec![0.0_f32; source.len() * channels];
    for (i, s) in source.iter().enumerate() {
        input[i * channels + 4] = *s;
    }
    let out = stretch(&input, channels, 1.25, 1152);
    let energy = |channel: usize| -> f64 {
        out.iter()
            .skip(channel)
            .step_by(channels)
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum()
    };
    assert!(energy(4) > 1000.0);
    for channel in (0..channels).filter(|c| *c != 4) {
        assert!(
            energy(channel) < energy(4) * 1e-6,
            "channel {channel} leaks"
        );
    }
}

/// A stream shorter than the pre-roll still comes out, at its length, and an
/// empty one gives nothing.
#[test]
fn a_very_short_stream_comes_out() {
    let input = tone(440.0, 0.01, 2, 0.3);
    let out = stretch(&input, 2, 1.25, 1152);
    assert_eq!(
        out.len() / 2,
        (input.len() as f64 / 2.0 / 1.25).round() as usize
    );
    assert!(stretch(&[], 2, 1.25, 1152).is_empty());
}

/// After a seek the stream starts over as new: nothing of the old position, and
/// the same frames for the same input.
#[test]
fn reset_starts_over() {
    let input = tone(440.0, 1.5, 2, 0.3);
    let fresh = stretch(&input, 2, 1.25, 1152);

    let mut s = Stretcher::new(RATE, 2, 1.25).unwrap();
    let mut discarded = Vec::new();
    s.process(&tone(1000.0, 1.0, 2, 0.5), &mut discarded)
        .unwrap();
    s.reset().unwrap();
    let mut out = Vec::new();
    for c in input.chunks(1152 * 2) {
        s.process(c, &mut out).unwrap();
    }
    s.finish(&mut out).unwrap();
    assert_eq!(out.len(), fresh.len());
    let worst = out
        .iter()
        .zip(&fresh)
        .fold(0.0_f32, |w, (a, b)| w.max((a - b).abs()));
    assert!(worst < 0.02, "worst difference {worst}");
}

/// Full-scale noise: a phase vocoder pushes its peaks far past 1.
fn loud_noise(seconds: f64, channels: usize) -> Vec<f32> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut out = Vec::with_capacity(frames * channels);
    for _ in 0..frames * channels {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state as f32 / u64::MAX as f32 * 2.0 - 1.0).signum());
    }
    out
}

#[test]
fn nothing_comes_out_over_the_ceiling() {
    let out = stretch(&loud_noise(2.0, 2), 2, 1.25, 1152);
    let peak = out.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
    assert!(peak <= LIMITER_CEILING + 1e-3, "peak {peak}");
}

/// What the gain after this stage will take down does not count.
#[test]
fn a_following_gain_leaves_the_peaks_alone() {
    let mut limiter = PeakLimiter::new(RATE, 1);
    limiter.set_output_gain(0.25);
    let input = vec![0.5_f32, 1.9, -1.9, 0.5];
    let mut out = Vec::new();
    limiter.process(&input, &mut out);
    limiter.flush(&mut out);
    assert_eq!(out, input);
}

/// Under the ceiling the limiter is the identity, to the bit, and it never
/// changes how many frames go through.
#[test]
fn the_limiter_touches_nothing_under_the_ceiling() {
    let input = tone(440.0, 0.5, 2, 0.9);
    let mut limiter = PeakLimiter::new(RATE, 2);
    let mut out = Vec::new();
    for c in input.chunks(333 * 2) {
        limiter.process(c, &mut out);
    }
    limiter.flush(&mut out);
    assert_eq!(out, input);
}

/// The gain comes down ahead of the peak and goes back up, so the frames around
/// a single peak are not clipped against it.
#[test]
fn the_limiter_eases_around_a_peak() {
    let mut input = vec![0.5_f32; RATE as usize * 2];
    input[RATE as usize / 2] = 1.96;
    let mut limiter = PeakLimiter::new(RATE, 1);
    let mut out = Vec::new();
    limiter.process(&input, &mut out);
    limiter.flush(&mut out);
    assert_eq!(out.len(), input.len());
    let at = RATE as usize / 2;
    assert!(out[at] <= LIMITER_CEILING + 1e-3, "peak {}", out[at]);
    // The ramp is smooth: no frame moves the gain by more than a look-ahead step.
    let gains: Vec<f32> = out.iter().zip(&input).map(|(o, i)| o / i).collect();
    let step = 0.5 / (LIMITER_LOOKAHEAD_MS / 1000.0 * f64::from(RATE)) as f32;
    for pair in gains.windows(2) {
        assert!((pair[1] - pair[0]).abs() <= step * 1.5, "step {pair:?}");
    }
    // Far from the peak nothing is touched.
    assert_eq!(out[at / 2], 0.5);
    assert!((out[at + RATE as usize] - 0.5).abs() < 1e-3);
}
