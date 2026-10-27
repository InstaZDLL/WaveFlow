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

/// The same seed gives the same noise: two outputs started together
/// dither a given stream identically, which keeps a reopened stream's
/// output reproducible.
#[test]
fn two_dithers_from_new_agree() {
    let mut a = Dither16::new();
    let mut b = Dither16::new();
    for n in 0..10_000 {
        let sample = (n as f32 * 0.37).sin() * 0.3 + 1e-5;
        assert_eq!(a.quantize(sample), b.quantize(sample));
    }
}

/// xorshift32 never reaches zero from a non-zero seed (zero would be a
/// fixed point, and the noise would stop for good).
#[test]
fn the_noise_state_never_reaches_zero() {
    let mut dither = Dither16::new();
    for _ in 0..1_000_000 {
        dither.unit();
        assert_ne!(dither.state, 0);
    }
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
