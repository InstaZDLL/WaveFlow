use super::*;

/// The real route on the machine running the test: the file leaves
/// its folder. On CI that is the freedesktop and Windows trash.
#[tokio::test]
async fn a_file_leaves_for_the_trash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("copy.mp3");
    std::fs::write(&path, b"not really audio").expect("write");
    let moved = path.clone();
    let outcome = on_trash_thread(move || move_one(&moved))
        .await
        .expect("thread");
    assert_eq!(outcome, Ok(()));
    assert!(!path.exists());
}

/// A file that cannot be found was not moved, even in a folder that
/// is there: the folder may be the empty mount point of an unplugged
/// drive (a library at the volume's root), so its row must stay.
#[tokio::test]
async fn a_missing_file_is_not_reported_as_moved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gone = dir.path().join("gone.mp3");
    let outcome = on_trash_thread(move || move_one(&gone))
        .await
        .expect("thread");
    assert!(outcome.is_err(), "{outcome:?}");

    let unplugged = dir.path().join("unplugged").join("copy.mp3");
    let outcome = on_trash_thread(move || move_one(&unplugged))
        .await
        .expect("thread");
    assert!(outcome.is_err(), "{outcome:?}");
}
