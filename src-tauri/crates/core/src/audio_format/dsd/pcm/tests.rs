use super::*;
use crate::audio_format::dsd::parser::{DsdChannels, DsdContainer, DsdLayout};

fn dsd64_stereo(block_interleave: Option<u32>, lsb_first: bool) -> DsdLayout {
    DsdLayout {
        container: if block_interleave.is_some() {
            DsdContainer::Dsf
        } else {
            DsdContainer::Dff
        },
        channels: DsdChannels::Stereo,
        sample_rate_hz: 2_822_400,
        samples_per_channel: 2_822_400,
        data_offset: 0,
        data_len_bytes: 0,
        block_interleave,
        lsb_first,
    }
}

#[test]
fn output_rate_is_input_rate_over_decimation() {
    let layout = dsd64_stereo(None, false);
    let pcm = DsdToPcm::new(&layout);
    assert_eq!(pcm.output_rate_hz, 44_100);
}

#[test]
fn dff_byte_interleaved_silence_stays_silent() {
    // All-zeros DSD bitstream → constant -1.0 → after the FIR
    // settles, the PCM output should be flat at -1.0 (DC). We
    // don't probe the transient (the first ~taps/decimation
    // samples while the FIR fills its history), but late values
    // must be tightly bounded.
    let layout = dsd64_stereo(None, false);
    let mut pcm = DsdToPcm::new(&layout);
    let bytes = vec![0u8; 1024];
    let mut out = Vec::new();
    pcm.decode_block(&bytes, &mut out);
    assert_eq!(out.len() % 2, 0, "must produce whole stereo frames");
    // After enough samples to fully prime the FIR, every output
    // should sit close to -1.0 (DC of the all-zeros input).
    let primed_start = (FILTER_TAPS / DECIMATION + 2) * 2;
    if out.len() > primed_start {
        for &s in &out[primed_start..] {
            assert!(
                (s + 1.0).abs() < 0.01,
                "expected ~-1.0 once filter primed, got {s}"
            );
        }
    }
}

#[test]
fn dff_alternating_bits_stay_bounded() {
    // 0x55 = 0b01010101 → square wave at Nyquist (the worst case
    // for a low-pass). After filtering it should land near zero
    // with a bounded magnitude — definitely not ±1.
    let layout = dsd64_stereo(None, false);
    let mut pcm = DsdToPcm::new(&layout);
    let bytes = vec![0x55u8; 8192];
    let mut out = Vec::new();
    pcm.decode_block(&bytes, &mut out);
    let primed_start = FILTER_TAPS;
    if out.len() > primed_start {
        for &s in &out[primed_start..] {
            assert!(
                s.abs() < 0.5,
                "Nyquist-frequency input should be heavily attenuated, got {s}"
            );
            assert!(s.is_finite());
        }
    }
}

#[test]
fn dsf_block_interleaved_decodes_without_panic() {
    let layout = dsd64_stereo(Some(4096), true);
    let mut pcm = DsdToPcm::new(&layout);
    // Two blocks worth: 4096 bytes for ch0 followed by 4096 for
    // ch1, then repeat.
    let bytes = vec![0xAAu8; 4096 * 2 * 2];
    let mut out = Vec::new();
    pcm.decode_block(&bytes, &mut out);
    assert!(!out.is_empty(), "should emit some PCM");
    assert_eq!(out.len() % 2, 0, "must produce whole stereo frames");
}

#[test]
fn new_with_taps_clamps_below_floor() {
    // A bogus low tap count must not disarm the anti-alias filter —
    // it's clamped up to MIN_FILTER_TAPS.
    let layout = dsd64_stereo(None, false);
    let pcm = DsdToPcm::new_with_taps(&layout, 8);
    assert_eq!(pcm.taps, MIN_FILTER_TAPS);
    assert_eq!(pcm.coeffs.len(), MIN_FILTER_TAPS);
}

#[test]
fn high_tap_filter_still_attenuates_nyquist() {
    // The audiophile 2048-tap path must keep the same guarantee as
    // the default: a Nyquist-frequency square wave is crushed.
    let layout = dsd64_stereo(None, false);
    let mut pcm = DsdToPcm::new_with_taps(&layout, 2048);
    assert_eq!(pcm.taps, 2048);
    // 128 KiB of Nyquist square wave — enough stereo PCM frames to
    // run well past the longer 2048-tap prime window.
    let bytes = vec![0x55u8; 128 * 1024];
    let mut out = Vec::new();
    pcm.decode_block(&bytes, &mut out);
    // primed_start is in PCM samples; with more taps the FIR takes
    // longer to fill, so key off the actual tap count.
    let primed_start = pcm.taps;
    assert!(out.len() > primed_start, "expected output past the prime");
    for &s in &out[primed_start..] {
        assert!(s.abs() < 0.5, "Nyquist input should be attenuated, got {s}");
        assert!(s.is_finite());
    }
}

#[test]
fn fir_dc_gain_is_unity() {
    let coeffs = build_blackman_harris_lowpass(FILTER_TAPS, 0.5 / DECIMATION as f32);
    let sum: f32 = coeffs.iter().sum();
    assert!((sum - 1.0).abs() < 1e-5, "dc gain should be 1.0, got {sum}");
}

#[test]
fn read_bit_orderings() {
    // 0b10110010 = 0xB2
    // LSB-first reads bit indices 0..8 in time order:
    //   bit0=0, bit1=1, bit2=0, bit3=0, bit4=1, bit5=1, bit6=0, bit7=1
    for (i, expected) in [0, 1, 0, 0, 1, 1, 0, 1].iter().enumerate() {
        assert_eq!(read_bit(0xB2, i as u32, true), *expected);
    }
    // MSB-first reads bit 7 first:
    //   bit0=1, bit1=0, bit2=1, bit3=1, bit4=0, bit5=0, bit6=1, bit7=0
    for (i, expected) in [1, 0, 1, 1, 0, 0, 1, 0].iter().enumerate() {
        assert_eq!(read_bit(0xB2, i as u32, false), *expected);
    }
}
