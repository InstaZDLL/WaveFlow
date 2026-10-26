//! Float samples back to the integer PCM an exclusive device takes.
//!
//! The decoder hands the output a float that symphonia made by dividing
//! an n-bit integer sample by 2^(n-1): `i16 / 32_768`, `i24 / 8_388_608`.
//! Multiplying by the same power of two and rounding gives every original
//! sample back unchanged, which is what bit-perfect output means.
//!
//! The exclusive backends used to multiply by 2^(n-1) - 1 and truncate.
//! That took one step off the magnitude of every non-zero sample, so a
//! stream badged "bit-perfect" was not, on any track.
//!
//! +1.0 has no integer counterpart (the positive range stops one step
//! short of the negative one), so it saturates to the largest value, as
//! any sample the chain pushed past full scale does. A NaN becomes
//! silence: the float-to-int cast maps it to 0.
//!
//! Shared by WASAPI and ALSA so the two cannot drift apart again, and
//! compiled on every system so the tests run on every CI runner, not only
//! on the one whose backend uses them.

/// A sample as 16-bit PCM.
#[inline]
pub fn to_i16(sample: f32) -> i16 {
    (sample * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16
}

/// TPDF dither for 16-bit output.
///
/// A sample the chain changed (volume, ReplayGain, the EQ, a fade) falls
/// between two 16-bit steps, and rounding it alone leaves an error that
/// follows the signal: at low level, on a fade-out, it is heard as
/// distortion rather than noise. Adding triangular noise of one step peak
/// before rounding turns that error into a steady, signal-independent hiss
/// at about -96 dBFS.
///
/// A sample that is already a 16-bit value is passed through untouched:
/// that is every sample of a 16-bit source nothing has altered, so the
/// bit-perfect path stays bit-perfect. 24- and 32-bit output need no
/// dither: their step sits below any converter's own noise.
///
/// The noise comes from a xorshift generator held by the render loop, so
/// the hot path neither allocates nor locks.
pub struct Dither16 {
    state: u32,
}

impl Dither16 {
    pub const fn new() -> Self {
        Self { state: 0x9E37_79B9 }
    }

    /// Uniform in [0, 1), from the top 24 bits of a xorshift32 step.
    #[inline]
    fn unit(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x >> 8) as f32 / 16_777_216.0
    }

    /// A sample as 16-bit PCM, dithered unless it already is one.
    #[inline]
    pub fn quantize(&mut self, sample: f32) -> i16 {
        let scaled = sample * 32_768.0;
        if scaled == scaled.round() {
            return to_i16(sample);
        }
        // The difference of two uniforms is triangular over (-1, 1) step.
        let noise = self.unit() - self.unit();
        (scaled + noise).round().clamp(-32_768.0, 32_767.0) as i16
    }
}

impl Default for Dither16 {
    fn default() -> Self {
        Self::new()
    }
}

/// A sample as 24-bit PCM, in the low 24 bits of an `i32`.
#[inline]
pub fn to_i24(sample: f32) -> i32 {
    (sample * 8_388_608.0)
        .round()
        .clamp(-8_388_608.0, 8_388_607.0) as i32
}

/// A sample as 32-bit PCM. Scaled in `f64`: an `f32` cannot hold every
/// 32-bit value, and a 16- or 24-bit source must land exactly on its
/// value shifted up.
#[inline]
pub fn to_i32(sample: f32) -> i32 {
    (f64::from(sample) * 2_147_483_648.0)
        .round()
        .clamp(-2_147_483_648.0, 2_147_483_647.0) as i32
}

#[cfg(test)]
mod tests;
