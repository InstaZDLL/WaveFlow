//! ITU-R BS.1770-4 loudness: K-weighted, gated, in LUFS.
//!
//! ## Why this replaced a plain RMS
//!
//! The first version of [`super::analyze_file`] measured
//! `10·log10(mean(s²))` over a mono sum and called the result a
//! loudness. That is a fine *relative* yardstick inside one library —
//! every track is measured the same wrong way — but it is not on the
//! same scale as anything the rest of the world writes into a file.
//! A `REPLAYGAIN_TRACK_GAIN` tag written by rsgain, foobar2000 or
//! beets is referenced to −18 LUFS **K-weighted and gated**; an
//! `R128_TRACK_GAIN` is referenced to −23 LUFS the same way. Reading
//! those tags (which we now do) while measuring our own tracks with an
//! unweighted RMS would put two sources on two scales, and the user
//! would hear the seam every time playback crossed from one to the
//! other. So the measurement had to move first.
//!
//! ## The algorithm, and where it can still be wrong
//!
//! Two biquads per channel (§2 of the spec): a high-shelf standing in
//! for the head's acoustic effect, then an RLB high-pass. Mean square
//! per channel over 400 ms blocks overlapping by 75 %, summed with the
//! per-channel weights, then **two gates** (§3): an absolute one at
//! −70 LUFS that throws away silence, and a relative one 10 LU below
//! the ungated mean that throws away the quiet passages so a track
//! isn't judged by its fade-out.
//!
//! The gates are the whole point and the reason this can't be a
//! running accumulator: the relative threshold isn't known until every
//! block has been measured, so block powers are kept and the mean is
//! taken twice. That costs one `f64` per 100 ms — about 24 KB for an
//! hour-long file, which is why it's a plain `Vec` and not something
//! cleverer.
//!
//! **Channel weights** come from the caller, which is the only place
//! that knows the layout. The spec weights the surround channels at
//! `G = 1.41` (+1.5 dB) and **excludes LFE entirely** — an action film
//! mixed with a loud LFE would otherwise measure far louder than it
//! sounds, and get too small a gain in return. Mono and stereo are all
//! `1.0`, so the common case is unaffected. A weight of `0.0` drops a
//! channel out of the sum, which is how LFE is excluded; a file that is
//! nothing but LFE therefore has no measurable loudness at all, which
//! is the honest answer.

/// Absolute gate, in LUFS (BS.1770-4 §3.1). Blocks quieter than this
/// are silence as far as the measurement is concerned.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// The relative gate sits this many LU below the ungated loudness
/// (BS.1770-4 §3.2).
const RELATIVE_GATE_LU: f64 = 10.0;
/// Calibration offset that makes a 1 kHz sine read its own dBFS level
/// — it cancels the K-weighting's gain at 1 kHz.
const CALIBRATION_OFFSET: f64 = -0.691;
/// Gating block length. The spec's `T_g`.
const BLOCK_MS: u64 = 400;
/// Blocks overlap by 75 %, so they advance one quarter-block at a
/// time and each 100 ms sub-block is counted in four consecutive
/// blocks.
const SUBBLOCKS_PER_BLOCK: usize = 4;

/// One biquad section in transposed direct form II.
///
/// Transposed rather than the textbook form because it carries its
/// state in two accumulators that already hold the running sums — one
/// multiply-add per stage, no delay line to shuffle — and it's the
/// numerically better-behaved of the two at `f64`.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Stage 1: the high-shelf that stands in for the head's
    /// acoustic effect.
    ///
    /// BS.1770-4 tabulates the coefficients at 48 kHz only. Rather
    /// than resample every file to 48 kHz just to measure it, the
    /// shelf is re-derived at the file's own rate from the analogue
    /// prototype the tabulated values come from (the same route
    /// libebur128 takes, and it reproduces the published 48 kHz
    /// numbers to the last digit — that's what the test below
    /// checks).
    fn k_shelf(sample_rate: f64) -> Self {
        // Prototype parameters behind the spec's 48 kHz table.
        const F0: f64 = 1681.974450955533;
        const GAIN_DB: f64 = 3.999843853973347;
        const Q: f64 = 0.7071752369554196;

        let k = (std::f64::consts::PI * F0 / sample_rate).tan();
        let vh = 10f64.powf(GAIN_DB / 20.0);
        // Not a typo and not `sqrt(vh)`: the shelf's mid-band gain
        // has its own exponent in the prototype.
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / Q + k * k;
        Self {
            b0: (vh + vb * k / Q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / Q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / Q + k * k) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Stage 2: the RLB high-pass. Its numerator is exactly
    /// `1, −2, 1` at every rate — only the poles move.
    fn rlb_highpass(sample_rate: f64) -> Self {
        const F0: f64 = 38.13547087602444;
        const Q: f64 = 0.5003270373238773;

        let k = (std::f64::consts::PI * F0 / sample_rate).tan();
        let a0 = 1.0 + k / Q + k * k;
        Self {
            b0: 1.0,
            b1: -2.0,
            b2: 1.0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / Q + k * k) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }
}

/// Streaming BS.1770-4 meter. Feed it interleaved frames as they
/// decode; ask for the number once at the end.
pub struct LoudnessMeter {
    /// Two filter stages per channel, each with its own state.
    filters: Vec<(Biquad, Biquad)>,
    /// Per-channel `G` from BS.1770-4 §2, in the buffer's own channel
    /// order. `0.0` excludes a channel (LFE).
    weights: Vec<f64>,
    channels: usize,
    /// Frames per 100 ms sub-block at this rate.
    subblock_frames: u64,
    /// Frames accumulated into the current sub-block.
    subblock_pos: u64,
    /// Sum of squared K-weighted samples in the current sub-block,
    /// per channel.
    subblock_sumsq: Vec<f64>,
    /// Mean square per channel for the last few completed sub-blocks
    /// — only the trailing `SUBBLOCKS_PER_BLOCK` matter, but a block
    /// is emitted as soon as that many exist, so this only ever grows
    /// by one entry per 100 ms and is trimmed in place.
    recent: Vec<Vec<f64>>,
    /// Weighted mean-square power of every completed 400 ms block.
    block_powers: Vec<f64>,
}

impl LoudnessMeter {
    /// A meter for `channels` interleaved channels at `sample_rate` Hz,
    /// every channel weighted `1.0`. Correct as-is for mono and stereo.
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self::with_channel_weights(sample_rate, &vec![1.0; channels.max(1)])
    }

    /// A meter with an explicit per-channel `G`, in the same order as
    /// the interleaved samples. See the module header for which
    /// channels the spec weights differently.
    pub fn with_channel_weights(sample_rate: u32, weights: &[f64]) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let weights: Vec<f64> = if weights.is_empty() {
            vec![1.0]
        } else {
            weights.to_vec()
        };
        let channels = weights.len();
        Self {
            filters: (0..channels)
                .map(|_| (Biquad::k_shelf(rate), Biquad::rlb_highpass(rate)))
                .collect(),
            weights,
            channels,
            // 100 ms — a quarter of the 400 ms block.
            subblock_frames: ((rate * (BLOCK_MS as f64 / 1000.0)) as u64
                / SUBBLOCKS_PER_BLOCK as u64)
                .max(1),
            subblock_pos: 0,
            subblock_sumsq: vec![0.0; channels],
            recent: Vec::with_capacity(SUBBLOCKS_PER_BLOCK),
            block_powers: Vec::new(),
        }
    }

    /// Feed one packet's worth of interleaved samples. A partial
    /// frame at the end of `samples` (a truncated packet) is ignored
    /// rather than zero-padded, which would inject a click into the
    /// filter state.
    pub fn push_interleaved(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            for (ch, &sample) in frame.iter().enumerate() {
                let (shelf, hpf) = &mut self.filters[ch];
                let y = hpf.process(shelf.process(f64::from(sample)));
                self.subblock_sumsq[ch] += y * y;
            }
            self.subblock_pos += 1;
            if self.subblock_pos >= self.subblock_frames {
                self.close_subblock();
            }
        }
    }

    fn close_subblock(&mut self) {
        let frames = self.subblock_pos as f64;
        let means: Vec<f64> = self
            .subblock_sumsq
            .iter()
            .map(|sumsq| sumsq / frames)
            .collect();
        self.subblock_sumsq.iter_mut().for_each(|s| *s = 0.0);
        self.subblock_pos = 0;

        self.recent.push(means);
        if self.recent.len() > SUBBLOCKS_PER_BLOCK {
            self.recent.remove(0);
        }
        if self.recent.len() == SUBBLOCKS_PER_BLOCK {
            // z_i averaged over the four sub-blocks, then summed
            // across channels with each channel's G.
            let power: f64 = (0..self.channels)
                .map(|ch| {
                    let z =
                        self.recent.iter().map(|m| m[ch]).sum::<f64>() / SUBBLOCKS_PER_BLOCK as f64;
                    self.weights[ch] * z
                })
                .sum();
            self.block_powers.push(power);
        }
    }

    /// Integrated loudness in LUFS, or `None` when nothing survived
    /// the absolute gate — a silent or shorter-than-400 ms file, for
    /// which no gain can honestly be suggested.
    pub fn finish(&self) -> Option<f64> {
        if self.block_powers.is_empty() {
            return None;
        }

        let absolute_floor = power_from_lufs(ABSOLUTE_GATE_LUFS);
        let above_absolute: Vec<f64> = self
            .block_powers
            .iter()
            .copied()
            .filter(|p| *p > absolute_floor)
            .collect();
        if above_absolute.is_empty() {
            return None;
        }

        // The relative gate is derived from the blocks that already
        // cleared the absolute one — gating on the raw mean would let
        // a long silence drag the threshold down.
        let ungated_mean = mean(&above_absolute);
        let relative_floor = power_from_lufs(lufs_from_power(ungated_mean) - RELATIVE_GATE_LU);
        let gated: Vec<f64> = above_absolute
            .into_iter()
            .filter(|p| *p > relative_floor)
            .collect();
        if gated.is_empty() {
            return None;
        }

        Some(lufs_from_power(mean(&gated)))
    }
}

#[inline]
fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

#[inline]
fn lufs_from_power(power: f64) -> f64 {
    if power > 0.0 {
        CALIBRATION_OFFSET + 10.0 * power.log10()
    } else {
        f64::NEG_INFINITY
    }
}

#[inline]
fn power_from_lufs(lufs: f64) -> f64 {
    10f64.powf((lufs - CALIBRATION_OFFSET) / 10.0)
}

#[cfg(test)]
mod tests;
