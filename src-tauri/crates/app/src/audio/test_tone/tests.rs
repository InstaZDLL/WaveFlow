use super::*;

const RATE: u32 = 48_000;

fn render(rate: u32, channels: usize) -> Vec<f32> {
    let mut buf = vec![0.0; total_frames(rate) as usize * channels + channels * 10];
    let frames = fill(&mut buf, 0, rate, channels);
    assert_eq!(frames as u64, total_frames(rate));
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
fn surround_channels_stay_silent_and_mono_gets_every_beep() {
    let buf = render(RATE, 6);
    for ch in 2..6 {
        assert_eq!(peak(buf.iter().skip(ch).step_by(6).copied()), 0.0);
    }
    let mono = render(RATE, 1);
    let slot = slot_frames(RATE) as usize;
    let beep = ms_to_frames(BEEP_MS, RATE) as usize;
    for start in [0, slot, 2 * slot] {
        assert!(peak(mono[start..start + beep].iter().copied()) > 0.1);
    }
}

#[test]
fn the_phase_changes_when_a_beep_starts() {
    let slot = slot_frames(RATE);
    assert_eq!(phase_at(0, RATE), Some(TonePhase::Left));
    assert_eq!(phase_at(slot - 1, RATE), Some(TonePhase::Left));
    assert_eq!(phase_at(slot, RATE), Some(TonePhase::Right));
    assert_eq!(phase_at(2 * slot, RATE), Some(TonePhase::Both));
    assert_eq!(phase_at(total_frames(RATE), RATE), None);
    assert_eq!(phase_at(0, 0), None);
}

#[test]
fn a_fill_resumes_where_the_last_one_stopped() {
    let whole = render(RATE, 2);
    let mut pieces = Vec::new();
    let mut frame = 0u64;
    let mut chunk = vec![0.0; 480 * 2];
    loop {
        let n = fill(&mut chunk, frame, RATE, 2);
        if n == 0 {
            break;
        }
        pieces.extend_from_slice(&chunk[..n * 2]);
        frame += n as u64;
    }
    assert_eq!(pieces, whole);
}
