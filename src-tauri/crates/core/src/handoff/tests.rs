use super::*;

#[test]
fn a_handoff_survives_the_trip_through_its_file() {
    let handoff = Handoff::new(
        vec![PathBuf::from("/m/a.flac"), PathBuf::from("/m/b.flac")],
        1,
        42_500,
        false,
    );
    let file = handoff.write().unwrap();
    assert!(is_handoff_file(&file));
    let read = Handoff::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    assert_eq!(read, handoff);
}

#[test]
fn the_file_is_found_after_the_flag_and_taken_from_the_working_directory() {
    let cwd = Path::new("/home/me");
    let args = ["waveflow", "--minimized", FLAG, "h.json"];
    assert_eq!(file_from_args(args, Some(cwd)), Some(cwd.join("h.json")));
    assert_eq!(file_from_args(["waveflow", "song.flac"], Some(cwd)), None);
    // The flag with nothing after it names no file.
    assert_eq!(file_from_args(["waveflow", FLAG], Some(cwd)), None);
}

#[test]
fn only_a_file_the_sender_wrote_may_be_removed() {
    let dir = tempfile::tempdir().unwrap();
    // The right name, in the wrong folder.
    assert!(!is_handoff_file(
        &dir.path().join("waveflow-handoff-1.json")
    ));
    // The right folder, a file of someone else's.
    assert!(!is_handoff_file(&std::env::temp_dir().join("notes.json")));
    assert!(!is_handoff_file(
        &std::env::temp_dir().join("waveflow-handoff-1.txt")
    ));
}

#[test]
fn a_newer_format_is_refused_and_an_added_field_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("h.json");
    std::fs::write(
        &file,
        r#"{"version":1,"paths":["/m/a.flac"],"index":0,"position_ms":0,"playing":true,"volume":0.5}"#,
    )
    .unwrap();
    assert!(Handoff::read(&file).is_ok());
    std::fs::write(
        &file,
        r#"{"version":2,"paths":["/m/a.flac"],"index":0,"position_ms":0,"playing":true}"#,
    )
    .unwrap();
    assert!(matches!(Handoff::read(&file), Err(HandoffError::Newer(2))));
}

#[test]
fn a_long_queue_is_cut() {
    let paths = (0..MAX_PATHS + 10)
        .map(|i| PathBuf::from(format!("/m/{i}.flac")))
        .collect();
    let file = Handoff::new(paths, 0, 0, true).write().unwrap();
    let read = Handoff::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    assert_eq!(read.paths.len(), MAX_PATHS);
}
