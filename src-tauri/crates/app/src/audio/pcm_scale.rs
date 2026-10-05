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

    /// The old scaling, kept as a witness of what this module fixes: it
    /// moved a plain mid-level sample.
    #[test]
    fn a_sample_the_old_scaling_altered() {
        let decoded = f32::from_sample(1000i16);
        assert_eq!((decoded * 32_767.0) as i16, 999);
        assert_eq!(to_i16(decoded), 1000);
    }
}
