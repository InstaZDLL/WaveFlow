//! DSD-to-PCM converter.
//!
//! Direct Stream Digital is a 1-bit sigma-delta bitstream at very
//! high rates (DSD64 = 2.8 MHz). Most of the energy above ~20 kHz
//! is high-frequency noise inherent to the encoding. To get
//! something a sound card can play, we need to:
//!
//!   1. Map each bit to ±1.0 (1 → +1.0, 0 → −1.0).
//!   2. Run the resulting stream through an aggressive low-pass FIR
//!      filter that cuts everything above the audible range.
//!   3. Decimate by an integer factor so the output rate becomes a
//!      sane PCM rate (DSD64 / 64 = 44.1 kHz, DSD128 / 64 = 88.2 kHz,
//!      etc.).
//!
//! The filter is a windowed-sinc with a Blackman-Harris envelope —
//! good stop-band attenuation (~92 dB) and modest taps (256). Not
//! the absolute state of the art (real audiophile players use multi-
//! stage halfband cascades), but enough to avoid audible artefacts
//! and keep the per-sample cost predictable.
//!
//! ## Streaming
//!
//! Callers feed [`DsdToPcm::decode_block`] DSD bytes (in whatever
//! interleave the container uses) and receive interleaved PCM f32
//! samples in [-1.0, +1.0]. Internal per-channel ring buffers hold
//! the FIR history across calls so the converter is fully streamable
//! — never reads the whole file at once.

use std::f32::consts::PI;

use super::parser::DsdLayout;

/// FIR filter length. 256 taps gives ~92 dB stop-band with a
/// Blackman-Harris window, which is enough to keep the DSD shaping
/// noise inaudible after decimation.
const FILTER_TAPS: usize = 256;

/// Decimation factor applied to the DSD bit rate. 64 is the canonical
/// "halve the rate twice and again" choice that lands DSD64 on
/// 44.1 kHz, DSD128 on 88.2 kHz, DSD256 on 176.4 kHz. Anything higher
/// rounds back to a standard PCM rate the rest of the audio engine
/// already handles.
const DECIMATION: usize = 64;

/// Streaming DSD → PCM converter.
///
/// Holds the FIR coefficient table once (shared across channels) and
/// per-channel circular buffers + decimation counters. Cheap to
/// construct: ~1 KB heap for a stereo decoder.
pub struct DsdToPcm {
    coeffs: Vec<f32>,
    channels: usize,
    /// Per-channel ring buffer storing the last `FILTER_TAPS` DSD
    /// samples as ±1.0 floats. Index `head` points at the oldest
    /// slot (about to be overwritten on next push).
    history: Vec<Vec<f32>>,
    head: Vec<usize>,
    /// Decimation counter per channel — increments on every new
    /// sample, emits one PCM output when it reaches `DECIMATION`.
    counter: Vec<usize>,
    /// PCM samples per channel still to be discarded before the
    /// converter starts emitting. Counts down from
    /// `FILTER_TAPS / DECIMATION` at init / reset so the first
    /// sample we actually push corresponds to a window 100% filled
    /// with real audio data — no residual transient from priming.
    /// Time cost: ~90 µs at DSD64, imperceptible.
    discard_outputs_remaining: Vec<usize>,
    /// Resulting PCM sample rate. Caller stamps it on the
    /// `ActiveStream` so the resampler knows what to convert from.
    pub output_rate_hz: u32,
    layout_lsb_first: bool,
    layout_block_interleave: Option<u32>,
}

impl DsdToPcm {
    pub fn new(layout: &DsdLayout) -> Self {
        let channels = layout.channels.count() as usize;
        let coeffs = build_blackman_harris_lowpass(FILTER_TAPS, 1.0 / DECIMATION as f32);
        // Discard the first FILTER_TAPS / DECIMATION outputs per
        // channel. After that many emissions the FIR window has
        // been fully overwritten with real audio bits and any bias
        // from the neutral priming is gone.
        let discard = FILTER_TAPS / DECIMATION;
        let mut me = Self {
            coeffs,
            channels,
            history: vec![vec![0.0; FILTER_TAPS]; channels],
            head: vec![0; channels],
            counter: vec![0; channels],
            discard_outputs_remaining: vec![discard; channels],
            output_rate_hz: layout.sample_rate_hz / DECIMATION as u32,
            layout_lsb_first: layout.lsb_first,
            layout_block_interleave: layout.block_interleave,
        };
        me.prime_neutral();
        me
    }

    /// Pre-fill the FIR history with a +1/-1 alternation. That
    /// pattern lives at exactly the Nyquist frequency of the DSD
    /// rate, which the low-pass filter crushes to (near-)zero — so
    /// any residual samples still in the ring on the first real
    /// `push_bit` contribute nothing to the convolution. Without
    /// this priming step the ring starts as zeros and the first
    /// FILTER_TAPS / DECIMATION ≈ 4 PCM outputs ramp from zero up
    /// to the real signal level, audible as a click at every track
    /// start — and very obvious during a crossfade where the
    /// outgoing stream is at full level.
    fn prime_neutral(&mut self) {
        for ch in 0..self.channels {
            for i in 0..FILTER_TAPS {
                self.history[ch][i] = if i & 1 == 0 { 1.0 } else { -1.0 };
            }
            // Head stays at 0 — the ring is conceptually full and
            // the next push_bit overwrites the oldest slot. Counter
            // stays at 0 so the first DECIMATION real bits trigger
            // the first PCM emission like a fresh start.
        }
    }

    /// Push one DSD bit (0/1) into channel `ch` and, when the
    /// decimation counter wraps, push the resulting PCM sample into
    /// `out`. PCM output is produced channel-by-channel, but the
    /// caller is expected to interleave by alternating push order
    /// across channels (handled by [`Self::decode_block`]).
    fn push_bit(&mut self, ch: usize, bit: u8, out: &mut Vec<f32>) {
        let sample = if bit == 0 { -1.0 } else { 1.0 };
        let head = self.head[ch];
        self.history[ch][head] = sample;
        self.head[ch] = (head + 1) % FILTER_TAPS;
        self.counter[ch] += 1;
        if self.counter[ch] >= DECIMATION {
            self.counter[ch] = 0;
            let pcm = self.convolve(ch);
            if self.discard_outputs_remaining[ch] > 0 {
                // Eat this output: it's still partially convolved
                // against neutral priming samples, not 100% real
                // audio, so it would carry a residual transient
                // audible at track start (especially during a
                // crossfade).
                self.discard_outputs_remaining[ch] -= 1;
            } else {
                out.push(pcm);
            }
        }
    }

    /// Convolve the FIR taps against the per-channel history. The
    /// ring buffer is unrolled into the natural time order: oldest
    /// sample first, paired with `coeffs[0]`.
    fn convolve(&self, ch: usize) -> f32 {
        let history = &self.history[ch];
        let head = self.head[ch];
        let mut acc = 0.0f32;
        for (i, &c) in self.coeffs.iter().enumerate() {
            // `head` points at the next write slot, so it's also the
            // oldest sample. Walk forward modulo the ring length.
            let idx = (head + i) % FILTER_TAPS;
            acc += c * history[idx];
        }
        acc
    }

    /// Decode `input` (raw DSD bytes laid out per `layout`) and
    /// append the resulting interleaved PCM samples to `out`.
    ///
    /// Two layout modes:
    ///
    /// - **DFF (byte-interleaved)**: bytes alternate channel by
    ///   channel — `[ch0, ch1, ch0, ch1, …]`. Each byte is consumed
    ///   bit-by-bit in MSB-first order.
    ///
    /// - **DSF (block-interleaved)**: input is laid out in fixed
    ///   `block_size` blocks per channel — all of channel 0, then
    ///   all of channel 1, repeating. Each byte is LSB-first.
    pub fn decode_block(&mut self, input: &[u8], out: &mut Vec<f32>) {
        match self.layout_block_interleave {
            Some(block_size) => self.decode_block_interleaved(input, block_size as usize, out),
            None => self.decode_byte_interleaved(input, out),
        }
    }

    fn decode_byte_interleaved(&mut self, input: &[u8], out: &mut Vec<f32>) {
        // Per-channel staging — bits decoded out of order land here,
        // and we emit a fully-interleaved PCM frame as soon as every
        // channel has produced one.
        let mut staged: Vec<Vec<f32>> = vec![Vec::new(); self.channels];
        for (i, &byte) in input.iter().enumerate() {
            let ch = i % self.channels;
            for bit_idx in 0..8 {
                let bit = read_bit(byte, bit_idx, self.layout_lsb_first);
                self.push_bit(ch, bit, &mut staged[ch]);
            }
        }
        interleave_into(&staged, self.channels, out);
    }

    fn decode_block_interleaved(
        &mut self,
        input: &[u8],
        block_size: usize,
        out: &mut Vec<f32>,
    ) {
        let stride = block_size * self.channels;
        let mut staged: Vec<Vec<f32>> = vec![Vec::new(); self.channels];
        for chunk in input.chunks(stride) {
            for ch in 0..self.channels {
                let start = ch * block_size;
                if start >= chunk.len() {
                    break;
                }
                let end = (start + block_size).min(chunk.len());
                for &byte in &chunk[start..end] {
                    for bit_idx in 0..8 {
                        let bit = read_bit(byte, bit_idx, self.layout_lsb_first);
                        self.push_bit(ch, bit, &mut staged[ch]);
                    }
                }
            }
        }
        interleave_into(&staged, self.channels, out);
    }
}

/// Extract bit `bit_idx` (0..8) from `byte`, in either LSB-first
/// (DSF: bit 0 is the earliest in time) or MSB-first (DFF: bit 7 is
/// the earliest) ordering. Returns 0 or 1.
fn read_bit(byte: u8, bit_idx: u32, lsb_first: bool) -> u8 {
    let shift = if lsb_first { bit_idx } else { 7 - bit_idx };
    (byte >> shift) & 1
}

/// Interleave per-channel PCM scratch buffers into one output frame
/// stream `[ch0[0], ch1[0], ch0[1], ch1[1], …]`. Trims to the
/// shortest channel so a partial block at the end of file doesn't
/// produce a lopsided frame.
fn interleave_into(staged: &[Vec<f32>], channels: usize, out: &mut Vec<f32>) {
    if staged.is_empty() {
        return;
    }
    let frames = staged.iter().map(Vec::len).min().unwrap_or(0);
    out.reserve(frames * channels);
    for f in 0..frames {
        for ch in 0..channels {
            out.push(staged[ch][f]);
        }
    }
}

/// Build a windowed-sinc FIR low-pass filter of `taps` coefficients
/// with normalised cutoff `cutoff` (0..0.5, where 0.5 = Nyquist).
/// Window is Blackman-Harris for ~92 dB stop-band rejection.
///
/// Coefficients are normalised so DC gain is exactly 1.0 — required
/// to keep loudness consistent regardless of decimation ratio.
fn build_blackman_harris_lowpass(taps: usize, cutoff: f32) -> Vec<f32> {
    let m = (taps - 1) as f32;
    let mut coeffs = vec![0.0f32; taps];
    for n in 0..taps {
        let nf = n as f32;
        // Sinc (centred on the middle tap)
        let x = nf - m / 2.0;
        let sinc = if x.abs() < 1e-9 {
            2.0 * cutoff
        } else {
            (2.0 * PI * cutoff * x).sin() / (PI * x)
        };
        // Blackman-Harris window
        let w = 0.35875
            - 0.48829 * (2.0 * PI * nf / m).cos()
            + 0.14128 * (4.0 * PI * nf / m).cos()
            - 0.01168 * (6.0 * PI * nf / m).cos();
        coeffs[n] = sinc * w;
    }
    let dc_gain: f32 = coeffs.iter().sum();
    if dc_gain.abs() > 1e-9 {
        for c in &mut coeffs {
            *c /= dc_gain;
        }
    }
    coeffs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsd::parser::{DsdChannels, DsdContainer, DsdLayout};

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
    fn fir_dc_gain_is_unity() {
        let coeffs = build_blackman_harris_lowpass(FILTER_TAPS, 1.0 / DECIMATION as f32);
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
}
