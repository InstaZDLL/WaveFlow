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
//! The 0x05 / 0xFA marker is (re)stamped **here**, per output frame, from
//! a single running counter the backend owns — see [`stamp_marker`] for
//! why the encoder's own marker can't be trusted to stay in cadence.
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

/// Payload mask: the 16 DSD bits under the marker.
const DOP_PAYLOAD_MASK: u32 = 0x0000_FFFF;

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

/// Stamp the marker for `phase` onto a payload word, replacing bits
/// 23..16 with `0x05` / `0xFA` by parity.
///
/// The backend — not the encoder — is the single source of the cadence.
/// A DoP DAC locks onto the *uninterrupted* 0x05 / 0xFA alternation
/// across the whole stream, and the encoder can't guarantee it: its own
/// phase restarts on `reset()` after every seek, and idle frames
/// generated here on pause / drain / underrun never pass through it at
/// all. Letting both sides number the frames independently would repeat
/// or skip a marker exactly at those boundaries — the DAC drops out of
/// DSD and plays the payload as PCM noise. Stamping every frame from one
/// running counter makes that structurally impossible.
#[inline]
pub fn stamp_marker(payload: u32, phase: u64) -> u32 {
    let marker: u32 = if phase & 1 == 0 {
        DOP_MARKER_A
    } else {
        DOP_MARKER_B
    };
    (marker << 16) | (payload & DOP_PAYLOAD_MASK)
}

/// A DoP idle (silence) word: the standard 0x69 DSD-silence payload in
/// both data bytes, with the marker chosen by `phase` parity. Decodes to
/// analog silence on the DAC while keeping it in DoP lock.
#[inline]
pub fn dop_silence_word(phase: u64) -> u32 {
    stamp_marker(DOP_SILENCE_PAYLOAD, phase)
}

/// Where one packed DoP sample lands. The implementations differ only in
/// the container the device wants — raw little-endian bytes (WASAPI) vs
/// 32-bit samples (ALSA `S32_LE`, CoreAudio) — never in the cadence,
/// which is why the fill / idle logic below is written once against this
/// trait and monomorphized per backend.
trait DopSink {
    fn write(&mut self, sample_idx: usize, word: u32);
}

struct ByteSink<'a> {
    padded: bool,
    bytes: &'a mut [u8],
}

impl DopSink for ByteSink<'_> {
    #[inline]
    fn write(&mut self, sample_idx: usize, word: u32) {
        write_dop_word(self.padded, word, self.bytes, sample_idx);
    }
}

struct I32Sink<'a> {
    out: &'a mut [i32],
}

impl DopSink for I32Sink<'_> {
    #[inline]
    fn write(&mut self, sample_idx: usize, word: u32) {
        self.out[sample_idx] = dop_word_to_i32(word);
    }
}

/// Write idle frames `from..to` into `sink`, advancing `phase` once per
/// frame. Every channel of a frame carries the same marker.
#[inline]
fn render_silence_into<S: DopSink>(
    sink: &mut S,
    channels: usize,
    from: usize,
    to: usize,
    phase: &mut u64,
) {
    for f in from..to {
        let word = dop_silence_word(*phase);
        *phase = phase.wrapping_add(1);
        for ch in 0..channels {
            sink.write(f * channels + ch, word);
        }
    }
}

/// Pull whole DoP frames from the ring into `sink` for one device
/// period, silence-filling the remainder on the first short frame so no
/// half-frame ever reaches the DAC. Returns the number of real samples
/// pulled (for `samples_played` crediting — underrun silence is
/// deliberately not counted, matching the PCM backends).
#[inline]
fn fill_period_into<S: DopSink>(
    sink: &mut S,
    channels: usize,
    need_frames: usize,
    consumer: &mut Consumer<f32>,
    phase: &mut u64,
) -> u64 {
    let mut written: u64 = 0;
    let mut f = 0usize;
    while f < need_frames {
        if consumer.slots() < channels {
            render_silence_into(sink, channels, f, need_frames, phase);
            break;
        }
        // One marker for the whole frame, from the same running counter
        // the idle frames use — the alternation survives underruns and
        // seeks (see `stamp_marker`).
        let marker_phase = *phase;
        *phase = phase.wrapping_add(1);
        for ch in 0..channels {
            // Slots checked above — this pop cannot fail.
            let payload = consumer.pop().map(|s| s.to_bits()).unwrap_or(0);
            sink.write(f * channels + ch, stamp_marker(payload, marker_phase));
        }
        written += channels as u64;
        f += 1;
    }
    written
}

/// Fill `bytes` with `need_frames` DoP idle frames, advancing `phase`
/// once per frame so the marker keeps alternating.
pub fn render_dop_silence(
    padded: bool,
    channels: usize,
    need_frames: usize,
    phase: &mut u64,
    bytes: &mut [u8],
) {
    render_silence_into(
        &mut ByteSink { padded, bytes },
        channels,
        0,
        need_frames,
        phase,
    );
}

/// Byte-container form of [`fill_period_into`] — the entire hot-loop body
/// the WASAPI backend runs each period; it owns only the device-specific
/// wait + write around it.
pub fn fill_dop_period(
    padded: bool,
    channels: usize,
    need_frames: usize,
    consumer: &mut Consumer<f32>,
    marker_phase: &mut u64,
    bytes: &mut [u8],
) -> u64 {
    fill_period_into(
        &mut ByteSink { padded, bytes },
        channels,
        need_frames,
        consumer,
        marker_phase,
    )
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
    marker_phase: &mut u64,
    out: &mut [i32],
) -> u64 {
    fill_period_into(
        &mut I32Sink { out },
        channels,
        need_frames,
        consumer,
        marker_phase,
    )
}

/// i32 / MSB-justified twin of [`render_dop_silence`].
pub fn render_dop_silence_i32(
    channels: usize,
    need_frames: usize,
    phase: &mut u64,
    out: &mut [i32],
) {
    render_silence_into(&mut I32Sink { out }, channels, 0, need_frames, phase);
}

#[cfg(test)]
mod tests;
