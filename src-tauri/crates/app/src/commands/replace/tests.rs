use super::*;

#[test]
fn a_reencode_is_the_same_length_and_an_edit_is_not() {
    assert!(same_length(240_000, 240_050));
    assert!(same_length(240_000, 238_100));
    // 1% of a long track is more than the 2 s floor.
    assert!(same_length(1_200_000, 1_211_000));
    assert!(!same_length(240_000, 250_000));
    assert!(!same_length(240_000, 236_000));
}

#[test]
fn the_new_file_takes_the_old_name_with_its_own_extension() {
    let target = target_for(Path::new("/m/a/01 Song.mp3"), Path::new("/dl/x.FLAC"));
    assert_eq!(target, Path::new("/m/a/01 Song.flac"));
}

/// The swap puts the new bytes at the target and the old file aside,
/// and the undo restores the old file exactly.
#[test]
fn a_swap_can_be_undone() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("song.mp3");
    let source = dir.path().join("better.flac");
    std::fs::write(&old, b"old bytes").unwrap();
    std::fs::write(&source, b"new bytes").unwrap();
    let target = target_for(&old, &source);

    let swap = prepare(&source, &old, &target, false).unwrap();
    place(&swap, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new bytes");
    assert!(!old.exists());
    assert_eq!(std::fs::read(&swap.aside).unwrap(), b"old bytes");
    assert!(source.exists(), "the user's file is copied, never moved");

    undo(&swap, &old, &target, true).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), b"old bytes");
    assert!(!target.exists());
    assert!(!dir.path().join(".waveflow").join("replaced").exists());
}

/// A file that appeared at the target after the check is not
/// overwritten, and the old file stays where it was.
#[test]
fn a_target_that_appeared_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("song.mp3");
    let source = dir.path().join("better.flac");
    let target = target_for(&old, &source);
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&source, b"new").unwrap();
    std::fs::write(&target, b"someone else's").unwrap();

    let swap = prepare(&source, &old, &target, false).unwrap();
    assert!(place(&swap, &target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"someone else's");
    undo(&swap, &old, &target, false).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), b"old");
    assert_eq!(std::fs::read(&target).unwrap(), b"someone else's");
}

/// Undoing never overwrites a file that appeared at the old path: the
/// old file then stays aside, and the error says where.
#[test]
fn an_undo_does_not_overwrite_a_file_that_appeared() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("song.flac");
    let source = dir.path().join("rerip.flac");
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&source, b"new").unwrap();
    let target = target_for(&old, &source);
    assert_eq!(target, old);

    let swap = prepare(&source, &old, &target, false).unwrap();
    std::fs::write(&old, b"someone else's").unwrap();
    let err = undo(&swap, &old, &target, false).unwrap_err();
    assert!(err.contains(&swap.aside.display().to_string()), "{err}");
    assert_eq!(std::fs::read(&old).unwrap(), b"someone else's");
    assert_eq!(std::fs::read(&swap.aside).unwrap(), b"old");
}

/// Same extension: the new file lands on the old one's own path.
#[test]
fn a_same_format_swap_reuses_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("song.flac");
    let source = dir.path().join("rerip.flac");
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&source, b"new").unwrap();
    let target = target_for(&old, &source);
    assert_eq!(target, old);

    let swap = prepare(&source, &old, &target, false).unwrap();
    place(&swap, &target).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), b"new");
    undo(&swap, &old, &target, true).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), b"old");
}

/// The better copy already at the target is adopted where it is: no
/// copy, and an undo leaves the user's file alone.
#[test]
fn an_adopted_file_is_never_copied_nor_removed() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("song.mp3");
    let source = dir.path().join("song.flac");
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&source, b"new").unwrap();
    let target = target_for(&old, &source);
    assert!(same_path(&source, &target));

    let swap = prepare(&source, &old, &target, true).unwrap();
    assert!(swap.staged.is_none());
    place(&swap, &target).unwrap();
    assert!(!old.exists());
    undo(&swap, &old, &target, true).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), b"old");
    assert_eq!(std::fs::read(&source).unwrap(), b"new");
}
