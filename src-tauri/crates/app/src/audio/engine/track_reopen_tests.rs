use super::super::output::DopFormat;
use super::rebuild::track_needs_reopen;

const DOP64: DopFormat = DopFormat {
    sample_rate: 176_400,
    channels: 2,
};

#[test]
fn a_pcm_track_after_a_matched_one_keeps_the_stream() {
    // The DoP check asks for no rate: the 48 kHz the last track
    // matched stays, and the rate match that follows finds it there.
    assert!(!track_needs_reopen(true, None, None, 48_000, None));
    assert!(!track_needs_reopen(true, None, None, 48_000, Some(48_000)));
}

#[test]
fn a_different_rate_reopens() {
    assert!(track_needs_reopen(true, None, None, 48_000, Some(44_100)));
    // The first match after an open that asked for nothing.
    assert!(track_needs_reopen(true, None, None, 0, Some(48_000)));
}

#[test]
fn dop_starting_stopping_or_missing_output_reopens() {
    assert!(track_needs_reopen(
        true,
        None,
        Some(DOP64),
        48_000,
        Some(176_400)
    ));
    assert!(track_needs_reopen(true, Some(DOP64), None, 176_400, None));
    assert!(track_needs_reopen(false, None, None, 0, None));
    assert!(!track_needs_reopen(
        true,
        Some(DOP64),
        Some(DOP64),
        176_400,
        Some(176_400)
    ));
}
