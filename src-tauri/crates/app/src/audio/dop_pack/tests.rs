use super::*;

#[test]
fn packed_word_is_three_le_bytes_marker_high() {
    // 0x05_12_34 → LE bytes [0x34, 0x12, 0x05].
    let mut b = vec![0u8; 3];
    write_dop_word(false, (0x05 << 16) | 0x1234, &mut b, 0);
    assert_eq!(b, vec![0x34, 0x12, 0x05]);
}

#[test]
fn padded_word_is_left_aligned_with_the_marker_in_the_top_byte() {
    // 24 valid bits in a 32-bit container are left-aligned
    // (WAVEFORMATEXTENSIBLE), so 0xFA_AB_CD is written as 0xFAABCD00 →
    // LE bytes [0x00, 0xCD, 0xAB, 0xFA].
    let mut b = vec![0u8; 4];
    write_dop_word(true, (0xFA << 16) | 0xABCD, &mut b, 0);
    assert_eq!(b, vec![0x00, 0xCD, 0xAB, 0xFA]);
    // The byte the DAC scans for the 0x05 / 0xFA cadence is the MSB; a
    // right-justified packing would leave 0x00 there and never lock.
    assert_eq!(b[3], 0xFA, "marker must be the most significant byte");
    assert_eq!(b[0], 0x00, "the unused bits must be the low ones");
}

#[test]
fn silence_word_alternates_marker_and_carries_idle_payload() {
    assert_eq!(dop_silence_word(0), (0x05 << 16) | 0x6969);
    assert_eq!(dop_silence_word(1), (0xFA << 16) | 0x6969);
    assert_eq!(dop_silence_word(2), (0x05 << 16) | 0x6969);
}

#[test]
fn render_silence_shares_marker_across_channels_and_advances_per_frame() {
    // 3 stereo frames → phases 0,1,2 → markers A,B,A on both channels.
    let mut phase = 0u64;
    let mut b = vec![0u8; 3 * 2 * 3]; // packed, stereo
    render_dop_silence(false, 2, 3, &mut phase, &mut b);
    let marker_at = |sample_idx: usize| b[sample_idx * 3 + 2];
    // Frame 0 (samples 0,1) → 0x05; frame 1 (2,3) → 0xFA; frame 2 → 0x05.
    assert_eq!(marker_at(0), 0x05);
    assert_eq!(marker_at(1), 0x05);
    assert_eq!(marker_at(2), 0xFA);
    assert_eq!(marker_at(3), 0xFA);
    assert_eq!(marker_at(4), 0x05);
    assert_eq!(marker_at(5), 0x05);
    assert_eq!(phase, 3);
}

#[test]
fn fill_period_underrun_silence_fills_the_rest() {
    // Empty ring → all frames become idle silence, zero real samples.
    let (_p, mut c) = rtrb::RingBuffer::<f32>::new(16);
    let mut phase = 0u64;
    let mut b = vec![0u8; 2 * 2 * 3];
    let written = fill_dop_period(false, 2, 2, &mut c, &mut phase, &mut b);
    assert_eq!(written, 0, "nothing in the ring → no real samples");
    // Every sample carries the idle payload.
    for s in 0..4 {
        assert_eq!(b[s * 3], 0x69);
        assert_eq!(b[s * 3 + 1], 0x69);
    }
    assert_eq!(phase, 2, "advanced one phase per silence frame");
}

#[test]
fn word_to_i32_puts_marker_in_the_top_byte() {
    // 0x05_12_34 → << 8 → 0x05_12_34_00. Marker (0x05) in bits 31..24.
    let s = dop_word_to_i32((0x05 << 16) | 0x1234);
    assert_eq!(s as u32, 0x0512_3400);
    assert_eq!((s as u32) >> 24, 0x05, "marker must be the MSB");
}

#[test]
fn fill_period_i32_pulls_then_silences_msb_justified() {
    let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(16);
    p.push(f32::from_bits((0x05 << 16) | 0x1111)).unwrap();
    p.push(f32::from_bits((0x05 << 16) | 0x2222)).unwrap();
    let mut phase = 0u64;
    let mut out = vec![0i32; 4]; // 2 stereo frames
    let written = fill_dop_period_i32(2, 2, &mut c, &mut phase, &mut out);
    assert_eq!(written, 2);
    assert_eq!(out[0] as u32, 0x0511_1100);
    assert_eq!(out[1] as u32, 0x0522_2200);
    // Frame 1 = idle silence, MSB-justified — and on the *next*
    // marker, because real and idle frames share one counter.
    assert_eq!(out[2] as u32, 0xFA69_6900);
    assert_eq!(out[3] as u32, 0xFA69_6900);
}

#[test]
fn marker_is_stamped_by_the_backend_not_taken_from_the_ring() {
    // The encoder's own marker is irrelevant: whatever sits in bits
    // 23..16 of a ring word is replaced by the running phase. Here
    // both frames arrive stamped 0x05; they must come out 0x05 then
    // 0xFA.
    let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(16);
    for w in [0x1111u32, 0x2222, 0x3333, 0x4444] {
        p.push(f32::from_bits((0x05 << 16) | w)).unwrap();
    }
    let mut phase = 0u64;
    let mut out = vec![0i32; 4];
    assert_eq!(fill_dop_period_i32(2, 2, &mut c, &mut phase, &mut out), 4);
    assert_eq!((out[0] as u32) >> 24, 0x05);
    assert_eq!((out[1] as u32) >> 24, 0x05, "both channels share a marker");
    assert_eq!((out[2] as u32) >> 24, 0xFA);
    assert_eq!((out[3] as u32) >> 24, 0xFA);
    // Payload untouched.
    assert_eq!(out[3] as u32 & 0x00FF_FF00, 0x0044_4400);
}

#[test]
fn cadence_survives_an_underrun_between_two_periods() {
    // Period 1: one real frame then an underrun idle frame.
    // Period 2: the ring refilled — the first real frame must carry
    // the marker that *continues* the alternation, not restart it.
    let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(16);
    p.push(f32::from_bits(0x0000_1111)).unwrap();
    p.push(f32::from_bits(0x0000_1111)).unwrap();
    let mut phase = 0u64;
    let mut out = vec![0i32; 4];
    fill_dop_period_i32(2, 2, &mut c, &mut phase, &mut out);
    let last = (out[2] as u32) >> 24; // idle frame's marker

    p.push(f32::from_bits(0x0000_2222)).unwrap();
    p.push(f32::from_bits(0x0000_2222)).unwrap();
    let mut out2 = vec![0i32; 2];
    fill_dop_period_i32(2, 1, &mut c, &mut phase, &mut out2);
    let next = (out2[0] as u32) >> 24;

    assert_ne!(last, next, "the marker must alternate across the seam");
    assert_eq!(next, 0x05, "phase 2 → marker A");
}

#[test]
fn fill_period_pulls_whole_frames_then_silence() {
    // Ring holds exactly one stereo frame of two DoP words.
    let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(16);
    let l = f32::from_bits((0x05 << 16) | 0x1111);
    let r = f32::from_bits((0x05 << 16) | 0x2222);
    p.push(l).unwrap();
    p.push(r).unwrap();
    let mut phase = 0u64;
    let mut b = vec![0u8; 2 * 2 * 3]; // room for 2 frames
    let written = fill_dop_period(false, 2, 2, &mut c, &mut phase, &mut b);
    assert_eq!(written, 2, "one full stereo frame pulled");
    // Frame 0 = the real words, stamped with marker A (phase 0).
    assert_eq!(&b[0..3], &[0x11, 0x11, 0x05]);
    assert_eq!(&b[3..6], &[0x22, 0x22, 0x05]);
    // Frame 1 = idle silence (ring now empty), marker B (phase 1).
    assert_eq!(&b[6..9], &[0x69, 0x69, 0xFA]);
    assert_eq!(&b[9..12], &[0x69, 0x69, 0xFA]);
}
