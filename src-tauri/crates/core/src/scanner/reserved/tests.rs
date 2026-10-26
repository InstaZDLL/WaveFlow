use super::*;
use std::path::PathBuf;

#[test]
fn a_clip_under_the_reserved_directory_is_ours() {
    let root = PathBuf::from("/music");
    assert!(is_in_reserved_dir(
        &root,
        Path::new("/music/.waveflow/canvas/a.mp4")
    ));
    // Nested deeper, and directly at the root.
    assert!(is_in_reserved_dir(
        &root,
        Path::new("/music/Artist/.waveflow/x.mp4")
    ));
    assert!(is_in_reserved_dir(&root, Path::new("/music/.waveflow")));
}

#[test]
fn ordinary_music_is_not() {
    let root = PathBuf::from("/music");
    assert!(!is_in_reserved_dir(
        &root,
        Path::new("/music/Artist/Album/01.flac")
    ));
    // A name that merely starts the same way is a different directory.
    assert!(!is_in_reserved_dir(
        &root,
        Path::new("/music/.waveflow-old/x.mp4")
    ));
}

/// The reason the check is rooted: a library that lives *under* a
/// directory with the reserved name is still an ordinary library, and
/// excluding all of it would leave the user with nothing and no
/// explanation.
#[test]
fn a_library_that_sits_inside_such_a_directory_still_scans() {
    let root = PathBuf::from("/home/u/.waveflow/Music");
    assert!(!is_in_reserved_dir(
        &root,
        Path::new("/home/u/.waveflow/Music/Artist/01.flac")
    ));
    assert!(is_in_reserved_dir(
        &root,
        Path::new("/home/u/.waveflow/Music/.waveflow/canvas/a.mp4")
    ));
}

/// Outside the root we cannot say, so we say no — for the watcher
/// that is one scan too many rather than a change it never saw.
#[test]
fn a_path_outside_the_root_is_not_claimed() {
    assert!(!is_in_reserved_dir(
        Path::new("/music"),
        Path::new("/elsewhere/.waveflow/a.mp4")
    ));
}
