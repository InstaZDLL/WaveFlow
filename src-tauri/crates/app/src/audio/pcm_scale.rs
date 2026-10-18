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
mod tests {
    use super::*;
    use symphonia::core::audio::conv::FromSample;
    use symphonia::core::audio::sample::i24;

    /// Every 16-bit value survives the trip through the decoder's own
    /// conversion and back, in both container widths it can end up in.
    #[test]
    fn every_16_bit_sample_comes_back_unchanged() {
        for v in i16::MIN..=i16::MAX {
            let decoded = f32::from_sample(v);
            assert_eq!(to_i16(decoded), v, "i16 {v}");
            assert_eq!(to_i24(decoded), i32::from(v) << 8, "i16 {v} as 24-bit");
            assert_eq!(to_i32(decoded), i32::from(v) << 16, "i16 {v} as 32-bit");
        }
    }

    /// Every 24-bit value survives too, as 24-bit and as 32-bit.
    #[test]
    fn every_24_bit_sample_comes_back_unchanged() {
        for v in -8_388_608..=8_388_607 {
            let decoded = f32::from_sample(i24(v));
            assert_eq!(to_i24(decoded), v, "i24 {v}");
            assert_eq!(to_i32(decoded), v << 8, "i24 {v} as 32-bit");
        }
    }

    #[test]
    fn full_scale_and_beyond_saturate() {
        assert_eq!(to_i16(1.0), i16::MAX);
        assert_eq!(to_i16(-1.0), i16::MIN);
        assert_eq!(to_i16(3.0), i16::MAX);
        assert_eq!(to_i16(-3.0), i16::MIN);
        assert_eq!(to_i24(1.0), 8_388_607);
        assert_eq!(to_i24(-1.0), -8_388_608);
        assert_eq!(to_i32(1.0), i32::MAX);
        assert_eq!(to_i32(-1.0), i32::MIN);
        assert_eq!(to_i16(f32::NAN), 0);
        assert_eq!(to_i24(f32::NAN), 0);
        assert_eq!(to_i32(f32::NAN), 0);
    }

    /// Dither never touches a sample that is already a 16-bit value, so
    /// an unaltered 16-bit stream still comes back bit for bit.
    #[test]
    fn dither_leaves_every_16_bit_sample_unchanged() {
        let mut dither = Dither16::new();
        for v in i16::MIN..=i16::MAX {
            let decoded = f32::from_sample(v);
            assert_eq!(dither.quantize(decoded), v, "i16 {v}");
        }
        assert_eq!(dither.quantize(0.0), 0);
        assert_eq!(dither.quantize(3.0), i16::MAX);
        assert_eq!(dither.quantize(f32::NAN), 0);
    }

    /// A sample between two steps lands on one of its neighbours, and on
    /// average where it really is: the dither adds noise, not a bias.
    #[test]
    fn dither_is_bounded_and_unbiased() {
        let mut dither = Dither16::new();
        let target = 1000.3_f32;
        let sample = target / 32_768.0;
        let runs = 200_000;
        let mut sum = 0.0_f64;
        for _ in 0..runs {
            let v = dither.quantize(sample);
            assert!((999..=1002).contains(&v), "{v}");
            sum += f64::from(v);
        }
        let mean = sum / f64::from(runs);
        assert!((mean - f64::from(target)).abs() < 0.01, "mean {mean}");
    }

    /// The old scaling, kept as a witness of what this module fixes: it
    /// moved a plain mid-level sample.
    #[test]
    fn a_sample_the_old_scaling_altered() {
        let decoded = f32::from_sample(1000i16);
        assert_eq!((decoded * 32_767.0) as i16, 999);
        assert_eq!(to_i16(decoded), 1000);
    }
}
