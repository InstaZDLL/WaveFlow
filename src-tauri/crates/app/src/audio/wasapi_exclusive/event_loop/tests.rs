use super::*;

#[test]
fn pack_samples_float32_round_trips() {
    let samples = [0.0_f32, 1.0, -1.0, 0.5];
    let mut bytes = vec![0u8; samples.len() * 4];
    pack_samples(
        ExclusiveSampleFormat::Float32,
        &samples,
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    for (i, sample) in samples.iter().enumerate() {
        let round = f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(round, *sample);
    }
}

#[test]
fn pack_samples_s16_saturates_and_round_trips() {
    let samples = [0.0_f32, 1.0, -1.0, 0.5, 2.0, -2.0];
    let mut bytes = vec![0u8; samples.len() * 2];
    pack_samples(
        ExclusiveSampleFormat::Pcm16,
        &samples,
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    let extract = |i: usize| i16::from_le_bytes(bytes[i * 2..i * 2 + 2].try_into().unwrap());
    assert_eq!(extract(0), 0);
    assert_eq!(extract(1), 32_767); // +1.0 has no 16-bit value: saturates
    assert_eq!(extract(2), -32_768);
    assert_eq!(extract(3), 16_384); // exactly 0.5 * 2^15
    assert_eq!(extract(4), 32_767); // saturated above 1.0
    assert_eq!(extract(5), -32_768); // saturated below -1.0
}

#[test]
fn pack_samples_s24_packed_three_bytes_per_sample() {
    let samples = [0.0_f32, 1.0, -1.0];
    let mut bytes = vec![0u8; samples.len() * 3];
    pack_samples(
        ExclusiveSampleFormat::Pcm24Packed,
        &samples,
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    // 0.0 → 0
    assert_eq!(&bytes[0..3], &[0, 0, 0]);
    // 1.0 → 8_388_607 = 0x7F_FF_FF → LE [0xFF, 0xFF, 0x7F]
    assert_eq!(&bytes[3..6], &[0xFF, 0xFF, 0x7F]);
    // -1.0 → -8_388_608 = signed i32 0xFF_80_00_00 → low 3 LE bytes
    let expected = (-8_388_608_i32).to_le_bytes();
    assert_eq!(&bytes[6..9], &expected[0..3]);
}

#[test]
fn pack_samples_s24_padded_is_left_aligned_in_the_container() {
    let samples = [1.0_f32, -1.0_f32, 0.0_f32];
    let mut bytes = vec![0u8; 12];
    pack_samples(
        ExclusiveSampleFormat::Pcm24Padded,
        &samples,
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    // WAVEFORMATEXTENSIBLE: the 24 valid bits sit in the MOST significant
    // part of the 32-bit container, unused low bits zeroed.
    assert_eq!(&bytes[0..4], &(8_388_607_i32 << 8).to_le_bytes());
    assert_eq!(&bytes[4..8], &((-8_388_608_i32) << 8).to_le_bytes());
    assert_eq!(&bytes[8..12], &[0, 0, 0, 0]);
    // The zeroed byte is the LOW one, not the high one — a right-justified
    // packing would put the zero at index 3 and drop the signal 48 dB.
    assert_eq!(bytes[0], 0, "unused bits must be the least significant");
    assert_eq!(bytes[3], 0x7F, "full scale must reach the top byte");
}
