use super::*;

#[test]
fn media_lives_under_the_reserved_directory() {
    let dir = media_dir_in(Path::new("/music"), MediaKind::Canvas);
    assert!(dir.ends_with(Path::new(".waveflow/canvas")), "{dir:?}");
    let dir = media_dir_in(Path::new("/music"), MediaKind::Motion);
    assert!(dir.ends_with(Path::new(".waveflow/motion")), "{dir:?}");
}

/// The library wins over a leftover copy in the profile directory, so
/// a user who moved their clips does not keep reading the old ones.
#[test]
fn the_first_candidate_that_exists_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    let profile = tmp.path().join("profile");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("abc.mp4"), b"x").unwrap();

    let candidates = vec![library.clone(), profile.clone()];
    assert_eq!(
        existing_media_file(&candidates, "abc", "mp4"),
        Some(profile.join("abc.mp4"))
    );

    std::fs::write(library.join("abc.mp4"), b"x").unwrap();
    assert_eq!(
        existing_media_file(&candidates, "abc", "mp4"),
        Some(library.join("abc.mp4"))
    );
    assert_eq!(existing_media_file(&candidates, "nope", "mp4"), None);
}
