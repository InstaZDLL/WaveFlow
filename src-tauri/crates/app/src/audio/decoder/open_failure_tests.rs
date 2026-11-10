use std::path::Path;

use super::open_failure_report;
use crate::audio::replay_gain::TrackGain;
use waveflow_core::playback::stream::ActiveStream;

/// The error the decoder gets back from its own open of `path`.
fn open_error(path: &Path) -> String {
    match ActiveStream::open(
        path,
        1,
        90_000,
        "library".into(),
        None,
        TrackGain::default(),
        0,
        false,
    ) {
        Ok(_) => panic!("{} opened", path.display()),
        Err(err) => err,
    }
}

#[test]
fn a_file_deleted_since_the_scan_is_reported_as_a_track_that_will_not_play() {
    // #1065: the first track of a queue just started reaches the decoder
    // like any other now, and its open is what tells the user the file is
    // gone — the toast a queue step onto the same file always raised.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Bravo.wav");
    std::fs::write(&path, b"RIFF").unwrap();
    std::fs::remove_file(&path).unwrap();

    let err = open_error(&path);
    let report = open_failure_report(&path, err.clone(), false).expect("reported");
    assert_eq!(report.kind, "track-failed");
    // The technical message rides along for the tooltip and the log.
    assert_eq!(report.message, err);
}

#[test]
fn a_share_that_is_down_is_reported_as_the_share_not_the_track() {
    // What `netlib::media::open` answers for a source known to be down,
    // which is how a blocked track the user picks reaches the decoder: the
    // same sentence the queue step's check used to raise on its own.
    let locator = Path::new("wfnet://11111111-aaaa/Music/2.wav");
    let report = open_failure_report(locator, crate::netlib::UNREACHABLE_MESSAGE.into(), false)
        .expect("reported");
    assert_eq!(report.kind, "network-source-lost");
}

#[test]
fn an_open_given_up_for_a_newer_command_reports_nothing() {
    // A newer load is already on its way: an error now would land over the
    // track the user picked after this one (#622).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.wav");
    let err = open_error(&path);
    assert!(open_failure_report(&path, err, true).is_none());

    let released = waveflow_core::playback::read_ahead::READ_RELEASED.to_string();
    let locator = Path::new("wfnet://11111111-aaaa/Music/2.wav");
    assert!(open_failure_report(locator, released, false).is_none());
}
