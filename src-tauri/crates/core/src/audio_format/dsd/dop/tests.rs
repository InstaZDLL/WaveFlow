use super::*;
use crate::audio_format::dsd::parser::{DsdChannels, DsdContainer, DsdLayout};

fn layout(rate: u32, block_interleave: Option<u32>, lsb_first: bool) -> DsdLayout {
    DsdLayout {
        container: if block_interleave.is_some() {
            DsdContainer::Dsf
        } else {
            DsdContainer::Dff
        },
        channels: DsdChannels::Stereo,
        sample_rate_hz: rate,
        samples_per_channel: rate as u64,
        data_offset: 0,
        data_len_bytes: 0,
        block_interleave,
        lsb_first,
    }
}

#[test]
fn output_rate_is_dsd_rate_over_sixteen() {
    // DSD64 → 176.4 kHz, DSD128 → 352.8, DSD256 → 705.6.
    assert_eq!(
        DsdToDop::new(&layout(2_822_400, None, false)).output_rate_hz,
        176_400
    );
    assert_eq!(
        DsdToDop::new(&layout(5_644_800, None, false)).output_rate_hz,
        352_800
    );
    assert_eq!(
        DsdToDop::new(&layout(11_289_600, None, false)).output_rate_hz,
        705_600
    );
}

#[test]
fn dff_bytes_are_packed_verbatim_msb_first() {
    // DFF byte-interleaved stereo: [L0, R0, L1, R1].
    // L payload = L0<<8 | L1, R payload = R0<<8 | R1, bytes verbatim.
    let mut enc = DsdToDop::new(&layout(2_822_400, None, false));
    let mut out = Vec::new();
    enc.encode_block(&[0x12, 0xAB, 0x34, 0xCD], &mut out);
    assert_eq!(out.len(), 2, "one stereo frame");
    // Frame 0 → marker A (0x05) on both channels.
    assert_eq!(out[0], (DOP_MARKER_A << 16) | 0x1234, "L: 0x05_12_34");
    assert_eq!(out[1], (DOP_MARKER_A << 16) | 0xABCD, "R: 0x05_AB_CD");
}

#[test]
fn dsf_bytes_are_bit_reversed() {
    // DSF is LSB-first: each byte must be reversed to MSB-first.
    // 0x01 → reverse_bits → 0x80. Block-interleaved, block_size 2:
    // [L0, L1, R0, R1].
    let mut enc = DsdToDop::new(&layout(2_822_400, Some(2), true));
    let mut out = Vec::new();
    enc.encode_block(&[0x01, 0x02, 0x03, 0x04], &mut out);
    assert_eq!(out.len(), 2);
    let rev = |b: u8| b.reverse_bits() as u32;
    assert_eq!(out[0], (DOP_MARKER_A << 16) | (rev(0x01) << 8) | rev(0x02));
    assert_eq!(out[1], (DOP_MARKER_A << 16) | (rev(0x03) << 8) | rev(0x04));
}

#[test]
fn marker_alternates_every_frame_and_is_shared_across_channels() {
    // Four stereo frames worth of DFF bytes → markers A,B,A,B, each
    // shared by L and R of the same frame.
    let mut enc = DsdToDop::new(&layout(2_822_400, None, false));
    let mut out = Vec::new();
    // 4 frames × 2 ch × 2 bytes = 16 bytes.
    enc.encode_block(&[0u8; 16], &mut out);
    assert_eq!(out.len(), 8, "4 stereo frames");
    let marker = |w: u32| w >> 16;
    for f in 0..4 {
        let expected = if f % 2 == 0 {
            DOP_MARKER_A
        } else {
            DOP_MARKER_B
        };
        assert_eq!(marker(out[f * 2]), expected, "L frame {f}");
        assert_eq!(marker(out[f * 2 + 1]), expected, "R frame {f}");
    }
}

#[test]
fn marker_cadence_survives_block_chunking() {
    // Encoding in two calls must produce the identical marker phase
    // to one call — the counter persists across calls.
    let bytes: Vec<u8> = (0..32).collect();
    let mut whole = Vec::new();
    DsdToDop::new(&layout(2_822_400, None, false)).encode_block(&bytes, &mut whole);

    let mut enc = DsdToDop::new(&layout(2_822_400, None, false));
    let mut split = Vec::new();
    enc.encode_block(&bytes[..12], &mut split);
    enc.encode_block(&bytes[12..], &mut split);

    assert_eq!(whole, split, "chunking must not change the output");
}

#[test]
fn odd_trailing_byte_is_carried_to_the_next_call() {
    // A channel gets an odd byte count on the first call; the leftover
    // must pair with the first byte of the next call, in time order.
    // DFF stereo, 6 bytes → 3 per channel → 1 full frame + 1 pending.
    let mut enc = DsdToDop::new(&layout(2_822_400, None, false));
    let mut out = Vec::new();
    enc.encode_block(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66], &mut out);
    assert_eq!(out.len(), 2, "only one complete frame so far");
    // L bytes seen: 0x11, 0x33 → frame0; 0x55 pending.
    assert_eq!(out[0], (DOP_MARKER_A << 16) | 0x1133);
    assert_eq!(out[1], (DOP_MARKER_A << 16) | 0x2244);

    // Next call supplies the pairs' second bytes.
    out.clear();
    enc.encode_block(&[0x77, 0x88], &mut out);
    assert_eq!(out.len(), 2, "the carried bytes complete a frame");
    // L: pending 0x55 + new 0x77 ; marker is now B (frame 1).
    assert_eq!(out[0], (DOP_MARKER_B << 16) | 0x5577);
    assert_eq!(out[1], (DOP_MARKER_B << 16) | 0x6688);
}

#[test]
fn reset_clears_pending_and_restarts_cadence() {
    let mut enc = DsdToDop::new(&layout(2_822_400, None, false));
    let mut out = Vec::new();
    // Leave a pending byte and advance the marker.
    enc.encode_block(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66], &mut out);
    enc.reset();
    out.clear();
    // After reset the pending 0x55/0x66 are gone and marker is A again.
    enc.encode_block(&[0xAA, 0xBB, 0xCC, 0xDD], &mut out);
    assert_eq!(
        out[0],
        (DOP_MARKER_A << 16) | 0xAACC,
        "L uses fresh bytes only"
    );
    assert_eq!(out[1], (DOP_MARKER_A << 16) | 0xBBDD);
}

#[test]
fn top_byte_above_the_marker_is_zero() {
    // The transport ships 3 bytes; bits 31..24 must stay clear so a
    // 32-bit reinterpretation never leaks garbage into the sample.
    let mut enc = DsdToDop::new(&layout(2_822_400, None, false));
    let mut out = Vec::new();
    enc.encode_block(&[0xFF, 0xFF, 0xFF, 0xFF], &mut out);
    for w in out {
        assert_eq!(w >> 24, 0, "bits 31..24 must be zero");
    }
}
