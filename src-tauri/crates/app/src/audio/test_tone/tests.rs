use super::*;

const RATE: u32 = 48_000;

fn sequence(channels: usize) -> ToneSequence {
    ToneSequence::new(&ChannelLayout::from_count(channels), false)
}

fn render(rate: u32, channels: usize) -> Vec<f32> {
    let tone = sequence(channels);
    let mut buf = vec![0.0; tone.total_frames(rate) as usize * channels + channels * 10];
    let frames = tone.fill(&mut buf, 0, rate);
    assert_eq!(frames as u64, tone.total_frames(rate));
    buf.truncate(frames * channels);
    buf
}

fn peak(samples: impl Iterator<Item = f32>) -> f32 {
    samples.fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn the_three_beeps_land_on_the_right_channels() {
    let buf = render(RATE, 2);
    let slot = slot_frames(RATE) as usize;
    let beep = ms_to_frames(BEEP_MS, RATE) as usize;
    let channel = |from: usize, to: usize, ch: usize| {
        peak(buf[from * 2..to * 2].iter().skip(ch).step_by(2).copied())
    };
    // Left beep: left only.
    assert!(channel(0, beep, 0) > 0.1);
    assert_eq!(channel(0, beep, 1), 0.0);
    // Right beep: right only.
    assert_eq!(channel(slot, slot + beep, 0), 0.0);
    assert!(channel(slot, slot + beep, 1) > 0.1);
    // Both.
    assert!(channel(2 * slot, 2 * slot + beep, 0) > 0.1);
    assert!(channel(2 * slot, 2 * slot + beep, 1) > 0.1);
    // The gaps are silent.
    assert_eq!(channel(beep, slot, 0), 0.0);
    assert_eq!(channel(beep, slot, 1), 0.0);
}

#[test]
fn it_never_exceeds_its_level_and_starts_and_ends_at_zero() {
    for rate in [44_100, 48_000, 96_000, 192_000] {
        let buf = render(rate, 2);
        assert!(peak(buf.iter().copied()) <= AMPLITUDE as f32 + 1e-6);
        assert!(buf[0].abs() < 1e-3 && buf[1].abs() < 1e-3);
        let n = buf.len();
        assert!(buf[n - 1].abs() < 1e-2 && buf[n - 2].abs() < 1e-2);
    }
}

#[test]
fn a_mono_output_gets_every_beep() {
    let mono = render(RATE, 1);
    let slot = slot_frames(RATE) as usize;
    let beep = ms_to_frames(BEEP_MS, RATE) as usize;
    for start in [0, slot, 2 * slot] {
        assert!(peak(mono[start..start + beep].iter().copied()) > 0.1);
    }
}

/// On a surround output each speaker has a beep of its own, and nothing else
/// sounds during it.
#[test]
fn each_speaker_of_a_surround_output_gets_a_beep_of_its_own() {
    for (channels, want) in [
        (
            6,
            vec![
                TonePhase::Left,
                TonePhase::Right,
                TonePhase::Center,
                TonePhase::Lfe,
                TonePhase::BackLeft,
                TonePhase::BackRight,
            ],
        ),
        (
            8,
            vec![
                TonePhase::Left,
                TonePhase::Right,
                TonePhase::Center,
                TonePhase::Lfe,
                TonePhase::BackLeft,
                TonePhase::BackRight,
                TonePhase::SideLeft,
                TonePhase::SideRight,
            ],
        ),
    ] {
        assert_eq!(sequence(channels).phases(), want);
        let buf = render(RATE, channels);
        let slot = slot_frames(RATE) as usize;
        let beep = ms_to_frames(BEEP_MS, RATE) as usize;
        for (n, frames) in buf.chunks(slot * channels).enumerate() {
            for ch in 0..channels {
                let level = peak(
                    frames[..beep * channels]
                        .iter()
                        .skip(ch)
                        .step_by(channels)
                        .copied(),
                );
                if ch == n {
                    assert!(level > 0.1, "beep {n}, channel {ch}");
                } else {
                    assert_eq!(level, 0.0, "beep {n}, channel {ch}");
                }
            }
        }
    }
}

/// The beeps follow the speakers, not the numbers: on an output in ALSA order
/// the centre's beep is on channel 4.
#[test]
fn the_beeps_find_the_speakers_of_an_output_in_another_order() {
    let tone = ToneSequence::new(&ChannelLayout::alsa_default(6), false);
    let channels = 6;
    let slot = slot_frames(RATE) as usize;
    let mut buf = vec![0.0; slot * channels];
    let at = 2 * slot as u64 + ms_to_frames(100, RATE);
    tone.fill(&mut buf, at, RATE);
    assert_eq!(tone.phase_at(at, RATE), Some(TonePhase::Center));
    assert!(peak(buf.iter().skip(4).step_by(channels).take(100).copied()) > 0.1);
    assert_eq!(
        peak(buf.iter().skip(2).step_by(channels).take(100).copied()),
        0.0
    );
}

/// A subwoofer has nothing to say at 1 kHz.
#[test]
fn the_lfe_beep_is_low_enough_for_a_subwoofer() {
    let buf = render(RATE, 6);
    let slot = slot_frames(RATE) as usize;
    let lfe: Vec<f32> = buf[3 * slot * 6..4 * slot * 6]
        .iter()
        .skip(3)
        .step_by(6)
        .copied()
        .collect();
    let beep = ms_to_frames(BEEP_MS, RATE) as usize;
    let crossings = lfe[..beep]
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let hz = crossings as f64 / (BEEP_MS as f64 / 1000.0);
    assert!((hz - LFE_FREQ_HZ).abs() < 5.0, "{hz} Hz");
}

/// With the mono switch on, the speakers past the front pair have nothing to
/// come out of: the card's warning says every beep comes out of both sides.
#[test]
fn front_only_keeps_the_three_stereo_beeps_on_a_wide_output() {
    let tone = ToneSequence::new(&ChannelLayout::from_count(8), true);
    assert_eq!(
        tone.phases(),
        [TonePhase::Left, TonePhase::Right, TonePhase::Both]
    );
    let frames = tone.total_frames(RATE) as usize;
    let mut buf = vec![0.0; frames * 8];
    tone.fill(&mut buf, 0, RATE);
    for ch in 2..8 {
        assert_eq!(peak(buf.iter().skip(ch).step_by(8).copied()), 0.0);
    }
}

#[test]
fn the_phase_changes_when_a_beep_starts() {
    let slot = slot_frames(RATE);
    let tone = sequence(2);
    assert_eq!(tone.phase_at(0, RATE), Some(TonePhase::Left));
    assert_eq!(tone.phase_at(slot - 1, RATE), Some(TonePhase::Left));
    assert_eq!(tone.phase_at(slot, RATE), Some(TonePhase::Right));
    assert_eq!(tone.phase_at(2 * slot, RATE), Some(TonePhase::Both));
    assert_eq!(tone.phase_at(tone.total_frames(RATE), RATE), None);
    assert_eq!(tone.phase_at(0, 0), None);
}

#[test]
fn a_fill_resumes_where_the_last_one_stopped() {
    let whole = render(RATE, 2);
    let tone = sequence(2);
    let mut pieces = Vec::new();
    let mut frame = 0u64;
    let mut chunk = vec![0.0; 480 * 2];
    loop {
        let n = tone.fill(&mut chunk, frame, RATE);
        if n == 0 {
            break;
        }
        pieces.extend_from_slice(&chunk[..n * 2]);
        frame += n as u64;
    }
    assert_eq!(pieces, whole);
}
