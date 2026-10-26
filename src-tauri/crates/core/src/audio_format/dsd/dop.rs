//! DSD → DoP (DSD over PCM) encoder.
//!
//! Where [`super::pcm`] *decodes* the 1-bit stream into audible PCM,
//! this module does the opposite of decoding: it leaves the DSD bits
//! untouched and merely **repackages** them inside 24-bit PCM frames so
//! they can travel down a bit-perfect PCM transport (WASAPI Exclusive)
//! to a DoP-capable DAC, which recognises the marker pattern, strips it,
//! and reconstructs the original 1-bit stream in its own hardware
//! sigma-delta modulator. The host performs no filtering, no volume, no
//! decimation — the sample that leaves here is the sample that hits the
//! DAC. That is the whole point: true native DSD, not DSD→PCM.
//!
//! ## Wire format (DoP Open Standard v1.1)
//!
//! Each 24-bit output sample carries **16 DSD bits** (two bytes) of one
//! channel plus an 8-bit marker:
//!
//! ```text
//!   bits 23..16 : marker byte, alternating 0x05 / 0xFA every frame
//!   bits 15..8  : first  (earlier-in-time) DSD byte
//!   bits  7..0  : second (later-in-time)   DSD byte
//! ```
//!
//! The marker toggles once per output frame and is **shared by every
//! channel of that frame** (L and R at frame `n` carry the same marker;
//! frame `n+1` carries the other). The DAC watches that alternation to
//! lock onto the DoP stream and to reject ordinary PCM (which would
//! never produce a stable 0x05/0xFA cadence in its top byte).
//!
//! Output sample rate is therefore the DSD bit rate divided by 16:
//! DSD64 (2.8224 MHz) → 176.4 kHz, DSD128 → 352.8 kHz, DSD256 →
//! 705.6 kHz. The transport must be opened at that exact rate in 24-bit,
//! which is why DoP only works over WASAPI Exclusive (the shared mixer
//! would resample and destroy the marker cadence).
//!
//! ## Bit order
//!
//! DoP defines the 16-bit payload as **MSB-first in time** (the DSD bit
//! that comes first in time is the most-significant bit of the field).
//! - **DFF** stores bytes MSB-first already → used verbatim.
//! - **DSF** stores bytes LSB-first → each byte is bit-reversed here so
//!   the earliest-in-time bit lands in the high position. Getting this
//!   wrong plays the stream time-reversed within every byte — audible
//!   as harsh noise on real hardware, so it is covered by tests.
//!
//! ## Streaming
//!
//! Like [`super::pcm::DsdToPcm`], the encoder is fully streamable: it
//! keeps per-channel leftover bytes and the marker phase across
//! [`DsdToDop::encode_block`] calls, so an arbitrary chunking of the
//! input produces the same output as one giant call.

use super::parser::DsdLayout;

/// DSD bits carried per DoP output sample (two bytes).
const DSD_BITS_PER_SAMPLE: u32 = 16;

/// The two DoP marker bytes, placed in bits 23..16 of each 24-bit word.
/// They alternate every output frame; the DAC locks onto the cadence.
const DOP_MARKER_A: u32 = 0x05;
const DOP_MARKER_B: u32 = 0xFA;

/// Streaming DSD → DoP repackager.
///
/// Holds no filter state (there is no filtering) — just enough to
/// de-interleave the container, pair bytes two-at-a-time per channel,
/// and keep the marker phase coherent across calls.
pub struct DsdToDop {
    channels: usize,
    /// Resulting 24-bit PCM sample rate (DSD bit rate / 16). Stamped on
    /// the stream so the transport is opened at the matching rate.
    pub output_rate_hz: u32,
    /// True when the container stores bytes LSB-first (DSF): each byte
    /// is bit-reversed so the DoP payload stays MSB-first in time.
    lsb_first: bool,
    /// `Some(block_size)` for DSF's per-channel block interleave, `None`
    /// for DFF's byte interleave. Mirrors [`DsdLayout::block_interleave`].
    block_interleave: Option<u32>,
    /// One leftover byte per channel, waiting for its pair from the next
    /// block. Even-sized reads never populate this; it exists for odd
    /// final reads and odd block sizes.
    pending: Vec<Option<u8>>,
    /// Global output-frame counter. Its parity picks the marker, so the
    /// cadence stays coherent no matter how the input was chunked.
    frame_counter: u64,
}

impl DsdToDop {
    /// Build a DoP encoder for `layout`. The output rate is fixed at
    /// `dsd_rate / 16`; the caller opens the exclusive device at that
    /// rate before feeding blocks.
    pub fn new(layout: &DsdLayout) -> Self {
        let channels = layout.channels.count() as usize;
        Self {
            channels,
            output_rate_hz: layout.sample_rate_hz / DSD_BITS_PER_SAMPLE,
            lsb_first: layout.lsb_first,
            block_interleave: layout.block_interleave,
            pending: vec![None; channels],
            frame_counter: 0,
        }
    }

    /// Number of channels the encoder interleaves.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Reset the streaming state (post-seek). Drops any half-formed
    /// sample and restarts the marker cadence — the DAC re-locks within
    /// a frame or two, inaudible.
    pub fn reset(&mut self) {
        self.pending.fill(None);
        self.frame_counter = 0;
    }

    /// Encode a chunk of raw DSD container bytes into interleaved 24-bit
    /// DoP samples, appended to `out`. Each value uses the low 24 bits;
    /// the top byte of a 32-bit word is left zero for the transport to
    /// ignore (it only ships three bytes per sample).
    ///
    /// The output length is always a whole number of frames
    /// (`channels` samples), so a downstream interleaved writer never
    /// sees a torn frame.
    pub fn encode_block(&mut self, input: &[u8], out: &mut Vec<u32>) {
        // De-interleave the container into one byte stream per channel,
        // prepending any leftover byte from the previous call.
        let per_channel = self.split_channels(input);

        // Pair bytes 2-at-a-time per channel into 16-bit payloads. Every
        // channel yields the same count for valid stereo/mono DSD, so we
        // key the frame count off the shortest to stay frame-aligned.
        let payloads: Vec<Vec<u16>> = per_channel
            .iter()
            .enumerate()
            .map(|(ch, bytes)| self.pack_payloads(ch, bytes))
            .collect();

        let frames = payloads.iter().map(Vec::len).min().unwrap_or(0);
        out.reserve(frames * self.channels);
        for f in 0..frames {
            let marker = if self.frame_counter & 1 == 0 {
                DOP_MARKER_A
            } else {
                DOP_MARKER_B
            };
            self.frame_counter += 1;
            for payload in payloads.iter() {
                // Safe: `frames` is the min length across channels.
                out.push((marker << 16) | payload[f] as u32);
            }
        }

        // Any byte a channel produced beyond `frames * 2` (only possible
        // when channels desynchronise on a truncated final read) is
        // dropped rather than carried — the stream is ending anyway and
        // half a DoP sample can't be shipped. In the common even-aligned
        // case there is nothing to drop.
    }

    /// De-interleave `input` into one `Vec<u8>` per channel, each led by
    /// this channel's leftover byte from the previous call (consumed).
    fn split_channels(&mut self, input: &[u8]) -> Vec<Vec<u8>> {
        let mut per_channel: Vec<Vec<u8>> = (0..self.channels)
            .map(|ch| {
                let mut v = Vec::new();
                if let Some(b) = self.pending[ch].take() {
                    v.push(b);
                }
                v
            })
            .collect();

        match self.block_interleave {
            // DSF: blocks of `block_size` bytes per channel, looping.
            Some(block_size) => {
                let block_size = block_size as usize;
                let stride = block_size * self.channels;
                for chunk in input.chunks(stride) {
                    for (ch, dst) in per_channel.iter_mut().enumerate() {
                        let start = ch * block_size;
                        if start >= chunk.len() {
                            break;
                        }
                        let end = (start + block_size).min(chunk.len());
                        dst.extend_from_slice(&chunk[start..end]);
                    }
                }
            }
            // DFF: bytes alternate channel by channel.
            None => {
                for (i, &byte) in input.iter().enumerate() {
                    per_channel[i % self.channels].push(byte);
                }
            }
        }

        per_channel
    }

    /// Pair a channel's byte stream into 16-bit DoP payloads, applying
    /// the MSB-first bit order. A trailing odd byte is stashed in
    /// `pending[ch]` for the next call.
    fn pack_payloads(&mut self, ch: usize, bytes: &[u8]) -> Vec<u16> {
        let mut payloads = Vec::with_capacity(bytes.len() / 2);
        let mut iter = bytes.chunks_exact(2);
        for pair in &mut iter {
            let hi = self.orient(pair[0]) as u16;
            let lo = self.orient(pair[1]) as u16;
            payloads.push((hi << 8) | lo);
        }
        // Carry a single trailing byte to be paired with the first byte
        // of the next block, preserving time order.
        if let [b] = iter.remainder() {
            self.pending[ch] = Some(*b);
        }
        payloads
    }

    /// Orient one DSD byte to MSB-first-in-time. DFF bytes are already
    /// MSB-first (verbatim); DSF bytes are LSB-first and get reversed.
    #[inline]
    fn orient(&self, byte: u8) -> u8 {
        if self.lsb_first {
            byte.reverse_bits()
        } else {
            byte
        }
    }
}

#[cfg(test)]
mod tests;
