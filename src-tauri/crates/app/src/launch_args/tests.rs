use super::*;

#[test]
fn the_program_and_its_options_are_not_files() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("song.flac");
    std::fs::write(&song, b"x").unwrap();
    let args = [
        OsString::from("waveflow"),
        OsString::from("--minimized"),
        song.clone().into_os_string(),
    ];
    assert_eq!(paths_from_args(args, None), [song]);
}

#[test]
fn a_relative_path_is_taken_from_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("song.flac"), b"x").unwrap();
    let args = ["waveflow", "song.flac"];
    assert_eq!(
        paths_from_args(args, Some(dir.path())),
        [dir.path().join("song.flac")]
    );
}

#[test]
fn dots_in_a_path_are_resolved_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("Music");
    std::fs::create_dir(&music).unwrap();
    std::fs::write(music.join("song.flac"), b"x").unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let args = ["waveflow", "../Music/./song.flac"];
    assert_eq!(
        paths_from_args(args, Some(&elsewhere)),
        [music.join("song.flac")]
    );
}

#[test]
fn files_that_arrive_together_are_gathered_once_each_in_order() {
    let a = PathBuf::from("/m/a.flac");
    let b = PathBuf::from("/m/b.flac");
    let c = PathBuf::from("/m/c.flac");
    let mut paths = Vec::new();
    gather(&mut paths, vec![a.clone(), b.clone()]);
    gather(&mut paths, vec![b.clone(), c.clone(), a.clone()]);
    assert_eq!(paths, [a, b, c]);
}

#[test]
fn one_launch_keeps_its_order_and_several_play_in_path_order() {
    let a = PathBuf::from("/m/Album/01 First.flac");
    let b = PathBuf::from("/m/Album/02 second.flac");
    let c = PathBuf::from("/m/Album/03 Third.flac");
    // `waveflow 03 01`: the order it was given.
    assert_eq!(
        play_order(vec![c.clone(), a.clone()], 1),
        [c.clone(), a.clone()]
    );
    // Three launches that reached the window third, second, first.
    assert_eq!(
        play_order(vec![c.clone(), b.clone(), a.clone()], 3),
        [a, b, c]
    );
}

#[test]
fn only_local_files_are_taken_from_opened_urls() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("my song.flac");
    std::fs::write(&song, b"x").unwrap();
    let urls = [
        url::Url::from_file_path(&song).unwrap(),
        url::Url::from_file_path(dir.path().join("gone.flac")).unwrap(),
        url::Url::parse("https://waveflow.app/").unwrap(),
    ];
    assert_eq!(paths_from_urls(&urls), [song]);
}

#[test]
fn a_file_uri_is_a_path_and_a_missing_file_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("my song.flac");
    std::fs::write(&song, b"x").unwrap();
    let uri = url::Url::from_file_path(&song).unwrap().to_string();
    let gone = dir.path().join("gone.flac").display().to_string();
    let args = vec!["waveflow".to_string(), uri, gone];
    assert_eq!(paths_from_args(args, None), [song]);
}
