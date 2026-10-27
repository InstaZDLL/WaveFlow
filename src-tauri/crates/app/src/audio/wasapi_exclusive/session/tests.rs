use super::*;

// ── same_wave_format ─────────────────────────────────────────────
//
// This decides whether the init retry runs at all: two shapes that
// compare equal skip the second attempt. A field the driver cares
// about but this comparison misses would silently swallow the
// retry, so each one gets its own case with a single field moved.

#[test]
fn same_wave_format_accepts_identical_formats() {
    let a = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 2);
    let b = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 2);
    assert!(same_wave_format(&a, &b));
}

/// The quirk that actually fixed #405: the probe only accepts the
/// format once a different `ksmedia.h` mask is swapped in. If that
/// difference were invisible here the retry would be skipped and
/// the fix would silently do nothing.
#[test]
fn same_wave_format_rejects_a_different_channel_mask() {
    let a = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 8);
    let mut b = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 8);
    b.wave_fmt.dwChannelMask ^= 0x1;
    assert!(!same_wave_format(&a, &b));
}

/// The other quirk: `WAVEFORMATEX` (`cbSize` 0) vs
/// `WAVEFORMATEXTENSIBLE` (`cbSize` 22), which the probe swaps for
/// mono/stereo. Same numbers, different structure — and drivers
/// genuinely accept one and refuse the other.
#[test]
fn same_wave_format_rejects_a_different_structure_size() {
    let a = ExclusiveSampleFormat::Pcm16.to_wave_format(44_100, 2);
    let mut b = ExclusiveSampleFormat::Pcm16.to_wave_format(44_100, 2);
    b.wave_fmt.Format.cbSize = 0;
    assert!(!same_wave_format(&a, &b));
}

#[test]
fn same_wave_format_rejects_a_different_channel_count() {
    let a = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 2);
    let b = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 8);
    assert!(!same_wave_format(&a, &b));
}

#[test]
fn same_wave_format_rejects_a_different_sample_rate() {
    let a = ExclusiveSampleFormat::Pcm16.to_wave_format(44_100, 2);
    let b = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 2);
    assert!(!same_wave_format(&a, &b));
}

#[test]
fn same_wave_format_rejects_a_different_bit_depth() {
    let a = ExclusiveSampleFormat::Pcm16.to_wave_format(48_000, 2);
    let b = ExclusiveSampleFormat::Pcm24Packed.to_wave_format(48_000, 2);
    assert!(!same_wave_format(&a, &b));
}

/// `S24_4LE` and `F32` share a 32-bit container and differ only in
/// the valid-bit count — the one field a container-size comparison
/// alone would miss.
#[test]
fn same_wave_format_rejects_a_different_valid_bit_count() {
    let padded = ExclusiveSampleFormat::Pcm24Padded.to_wave_format(48_000, 2);
    let float = ExclusiveSampleFormat::Float32.to_wave_format(48_000, 2);
    assert_eq!(
        padded.get_bitspersample(),
        float.get_bitspersample(),
        "precondition: both must use a 32-bit container"
    );
    assert!(!same_wave_format(&padded, &float));
}
