use super::*;

#[test]
fn a_permission_error_is_not_worth_waiting_out() {
    // The retry loop's whole job is telling these apart: waiting out
    // a scanner is right, waiting out "you may not have this file"
    // is a hang that ends in the same message anyway.
    let denied = io::Error::from_raw_os_error(5);
    assert!(!is_transient_lock(&denied));
}

#[cfg(windows)]
#[test]
fn a_sharing_violation_is() {
    assert!(is_transient_lock(&io::Error::from_raw_os_error(32)));
    assert!(is_transient_lock(&io::Error::from_raw_os_error(33)));
}

#[test]
fn the_body_sees_a_handle_it_can_write_through() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.bin");
    std::fs::write(&path, b"before").expect("seed");

    with_writable_file(&path, |file| -> Result<(), io::Error> {
        use std::io::{Seek, SeekFrom, Write};
        file.seek(SeekFrom::Start(0))?;
        file.write_all(b"after!")
    })
    .expect("write");

    assert_eq!(std::fs::read(&path).expect("read"), b"after!");
}

#[cfg(windows)]
#[test]
fn a_read_only_file_is_written_and_left_read_only() {
    // The reported stop: Windows refuses `write(true).open()` on a
    // read-only file, so the edit failed with a message describing
    // the symptom rather than the cause. Lifting it is ours to do;
    // keeping it lifted is not — the attribute is the user's.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("locked.bin");
    std::fs::write(&path, b"before").expect("seed");
    let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).expect("set read-only");

    with_writable_file(&path, |file| -> Result<(), io::Error> {
        use std::io::{Seek, SeekFrom, Write};
        file.seek(SeekFrom::Start(0))?;
        file.write_all(b"after!")
    })
    .expect("write");

    assert_eq!(std::fs::read(&path).expect("read"), b"after!");
    assert!(
        std::fs::metadata(&path)
            .expect("meta")
            .permissions()
            .readonly(),
        "the read-only attribute belongs to the user, not to the write"
    );
}

mod stamp_tests {
    use super::super::*;

    #[test]
    fn the_stamp_holds_while_the_file_does_not_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.flac");
        std::fs::write(&path, b"fLaC").unwrap();
        assert_eq!(file_stamp(&path).unwrap(), file_stamp(&path).unwrap());
    }

    #[test]
    fn a_rewrite_changes_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.flac");
        std::fs::write(&path, b"fLaC").unwrap();
        let before = file_stamp(&path).unwrap();
        std::fs::write(&path, b"fLaC with a longer tag").unwrap();
        assert_ne!(file_stamp(&path).unwrap(), before);
    }

    /// Same size, new content: what a tagger editing one field in the
    /// tag's padding produces. Only the time can tell it apart.
    #[test]
    fn a_same_size_rewrite_changes_it_through_the_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.flac");
        std::fs::write(&path, b"fLaC").unwrap();
        let before = file_stamp(&path).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        let later = std::fs::metadata(&path).unwrap().modified().unwrap() + Duration::from_secs(5);
        file.set_modified(later).unwrap();
        assert_ne!(file_stamp(&path).unwrap(), before);
    }

    #[test]
    fn a_missing_file_has_no_stamp() {
        let dir = tempfile::tempdir().unwrap();
        assert!(file_stamp(&dir.path().join("gone.flac")).is_err());
    }
}
