use super::*;

#[test]
fn unknown_format_is_off_rather_than_a_guess() {
    assert_eq!(TranscodeFormat::parse(None), TranscodeFormat::Off);
    assert_eq!(TranscodeFormat::parse(Some("off")), TranscodeFormat::Off);
    assert_eq!(TranscodeFormat::parse(Some("flac")), TranscodeFormat::Off);
    assert_eq!(TranscodeFormat::parse(Some("mp3")), TranscodeFormat::Mp3);
    assert_eq!(TranscodeFormat::parse(Some("opus")), TranscodeFormat::Opus);
}

#[test]
fn raw_carries_no_query_because_the_server_rejects_one() {
    // `format=raw` with a bitrate is a 400 server-side, so `Off` must
    // produce no query at all rather than an explicit raw.
    assert_eq!(TranscodeFormat::Off.query_value(), None);
}

#[test]
fn headroom_needs_both_a_transcoder_and_room() {
    let full = TranscodeStatus {
        available: true,
        active: 4,
        global_limit: 4,
        per_user_limit: 2,
    };
    assert!(!full.has_headroom(), "at the ceiling is not headroom");

    let no_ffmpeg = TranscodeStatus {
        available: false,
        active: 0,
        global_limit: 4,
        per_user_limit: 2,
    };
    assert!(!no_ffmpeg.has_headroom(), "idle but unable is not headroom");

    let ok = TranscodeStatus {
        available: true,
        active: 3,
        global_limit: 4,
        per_user_limit: 2,
    };
    assert!(ok.has_headroom());
}
