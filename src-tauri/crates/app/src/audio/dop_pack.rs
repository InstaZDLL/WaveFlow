//! Shared DoP (DSD over PCM) byte packing, #495.
//!
//! The DoP *words* — 24-bit values carrying an 8-bit marker plus two DSD
//! bytes — are produced by the decoder ([`waveflow_core::audio_format::dsd::dop::DsdToDop`])
//! and carried through the SPSC ring as `f32` bit patterns (a pure byte
//! pipe: `from_bits` on the way in, `to_bits` here, no arithmetic in
//! between). Every exclusive output backend — WASAPI (Windows), ALSA
//! (Linux), CoreAudio (macOS) — turns those words into the little-endian
//! device bytes and generates marker-carrying idle frames for pause /
//! drain / underrun the same way, so that logic lives here once instead
//! of being re-derived per platform.
//!
//! `padded` selects the 24-in-32 container (4 bytes, high byte 0) over
//! the compact 3-byte packing: WASAPI `Pcm24Padded`, ALSA `S32_LE`, and
//! CoreAudio's 32-bit path all want the former; WASAPI `Pcm24Packed` and
//! ALSA `S24_3LE` want the latter.

use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::Consumer;

/// The two DoP marker bytes, placed in bits 23..16 of each 24-bit word.
/// They alternate every output frame; the DAC locks onto the cadence.
const DOP_MARKER_A: u32 = 0x05;
const DOP_MARKER_B: u32 = 0xFA;

/// The DSD "digital silence" payload byte (0x69). Two of them under a
/// live marker decode to analog silence while keeping the DAC in DoP
/// lock — the correct thing to feed on pause / underrun.
const DOP_SILENCE_PAYLOAD: u32 = 0x6969;

/// Bytes per DoP sample on the wire for the two container shapes.
#[inline]
pub fn sample_bytes(padded: bool) -> usize {
    if padded {
        4
    } else {
        3
    }
}

/// Write one 24-bit DoP word at `sample_idx`, little-endian. `padded`
/// selects the 4-byte 24-in-32 container vs the compact 3-byte packing.
/// The word is masked to 24 bits — the marker sits in bits 23..16, the
/// two DSD bytes below it.
///
/// The two containers justify differently, and the marker's position is
/// what the DAC locks onto, so this is not cosmetic:
///
/// - **packed** (`wBitsPerSample = 24`): container *is* the word, written
///   as-is.
/// - **padded** (`wBitsPerSample = 32`, `wValidBitsPerSample = 24`):
///   WAVEFORMATEXTENSIBLE requires the valid bits **left-aligned**, unused
///   low bits zeroed, so the word is shifted up by 8 and the marker lands
///   in bits 31..24. Right-justifying it would leave 0x00 in the byte the
///   DAC scans for the 0x05 / 0xFA cadence — it would never lock, and the
///   DSD payload would be played as PCM noise.
#[inline]
pub fn write_dop_word(padded: bool, word: u32, bytes: &mut [u8], sample_idx: usize) {
    let v = word & 0x00FF_FFFF;
    if padded {
        let off = sample_idx * 4;
        bytes[off..off + 4].copy_from_slice(&(v << 8).to_le_bytes());
    } else {
        let off = sample_idx * 3;
        bytes[off] = v as u8;
        bytes[off + 1] = (v >> 8) as u8;
        bytes[off + 2] = (v >> 16) as u8;
    }
}

/// A DoP idle (silence) word: the standard 0x69 DSD-silence payload in
/// both data bytes, with the marker chosen by `phase` parity. Decodes to
/// analog silence on the DAC while keeping it in DoP lock.
#[inline]
pub fn dop_silence_word(phase: u64) -> u32 {
    let marker: u32 = if phase & 1 == 0 {
        DOP_MARKER_A
    } else {
        DOP_MARKER_B
    };
    (marker << 16) | DOP_SILENCE_PAYLOAD
}

/// Fill `bytes` with `need_frames` DoP idle frames, advancing `phase`
/// once per frame so the marker keeps alternating. Every channel of a
/// frame carries the same marker.
pub fn render_dop_silence(
    padded: bool,
    channels: usize,
    need_frames: usize,
    phase: &mut u64,
    bytes: &mut [u8],
) {
    for f in 0..need_frames {
        let word = dop_silence_word(*phase);
        *phase = phase.wrapping_add(1);
        for ch in 0..channels {
            write_dop_word(padded, word, bytes, f * channels + ch);
        }
    }
}

/// Pull whole DoP frames from the ring into `bytes` for one device
/// period, silence-filling the remainder on the first short frame so the
/// marker cadence never breaks mid-period. Returns the number of real
/// samples pulled (for `samples_played` crediting — underrun silence is
/// deliberately not counted, matching the PCM backends).
///
/// This is the entire hot-loop body every backend runs each period; the
/// backends own only the device-specific wait + write around it.
pub fn fill_dop_period(
    padded: bool,
    channels: usize,
    need_frames: usize,
    consumer: &mut Consumer<f32>,
    silence_phase: &mut u64,
    bytes: &mut [u8],
) -> u64 {
    let mut written: u64 = 0;
    let mut f = 0usize;
    while f < need_frames {
        if consumer.slots() < channels {
            for g in f..need_frames {
                let word = dop_silence_word(*silence_phase);
                *silence_phase = silence_phase.wrapping_add(1);
                for ch in 0..channels {
                    write_dop_word(padded, word, bytes, g * channels + ch);
                }
            }
            break;
        }
        for ch in 0..channels {
            // Slots checked above — this pop cannot fail.
            let word = consumer.pop().map(|s| s.to_bits()).unwrap_or(0);
            write_dop_word(padded, word, bytes, f * channels + ch);
        }
        written += channels as u64;
        f += 1;
    }
    written
}

/// A tiny helper so backends don't hand-roll `AtomicU64` juggling for the
/// silence phase when they keep it on the heap; most keep it as a plain
/// `u64` local instead. Provided for completeness / future use.
#[allow(dead_code)]
pub fn advance_phase(phase: &AtomicU64) -> u32 {
    let p = phase.fetch_add(1, Ordering::Relaxed);
    dop_silence_word(p) >> 16
}

// ── 32-bit MSB-justified path (ALSA S32_LE, CoreAudio 32-bit int) ─────
//
// Some transports present a full 32-bit sample to the DAC rather than a
// packed 24-bit one. There the DoP marker must land in the *most*
// significant byte (bits 31..24) for the DAC to detect the cadence, so
// the 24-bit word is left-shifted by 8. This matches WASAPI's
// `Pcm24Padded`, which is left-aligned for the same reason — the split
// here is i32-vs-bytes, not a difference in justification.

/// Place a 24-bit DoP word MSB-justified in a 32-bit sample: marker in
/// bits 31..24, the two DSD bytes in 23..8, low byte zero.
#[inline]
pub fn dop_word_to_i32(word: u32) -> i32 {
    ((word & 0x00FF_FFFF) << 8) as i32
}

/// i32 / MSB-justified twin of [`fill_dop_period`]. Writes into an `i32`
/// sample buffer (as `io_i32` / a CoreAudio 32-bit render buffer expect)
/// instead of raw bytes. Returns real samples pulled.
pub fn fill_dop_period_i32(
    channels: usize,
    need_frames: usize,
    consumer: &mut Consumer<f32>,
    silence_phase: &mut u64,
    out: &mut [i32],
) -> u64 {
    let mut written: u64 = 0;
    let mut f = 0usize;
    while f < need_frames {
        if consumer.slots() < channels {
            for g in f..need_frames {
                let s = dop_word_to_i32(dop_silence_word(*silence_phase));
                *silence_phase = silence_phase.wrapping_add(1);
                for ch in 0..channels {
                    out[g * channels + ch] = s;
                }
            }
            break;
        }
        for ch in 0..channels {
            let word = consumer.pop().map(|s| s.to_bits()).unwrap_or(0);
            out[f * channels + ch] = dop_word_to_i32(word);
        }
        written += channels as u64;
        f += 1;
    }
    written
}

/// i32 / MSB-justified twin of [`render_dop_silence`].
pub fn render_dop_silence_i32(
    channels: usize,
    need_frames: usize,
    phase: &mut u64,
    out: &mut [i32],
) {
    for f in 0..need_frames {
        let s = dop_word_to_i32(dop_silence_word(*phase));
        *phase = phase.wrapping_add(1);
        for ch in 0..channels {
            out[f * channels + ch] = s;
        }
    }
}

#[cfg(test)]
mod tests {
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
        // Frame 1 = idle silence, MSB-justified.
        assert_eq!(out[2] as u32, 0x0569_6900);
        assert_eq!(out[3] as u32, 0x0569_6900);
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
        // Frame 0 = the real words.
        assert_eq!(&b[0..3], &[0x11, 0x11, 0x05]);
        assert_eq!(&b[3..6], &[0x22, 0x22, 0x05]);
        // Frame 1 = idle silence (ring now empty).
        assert_eq!(&b[6..9], &[0x69, 0x69, 0x05]);
        assert_eq!(&b[9..12], &[0x69, 0x69, 0x05]);
    }
}
