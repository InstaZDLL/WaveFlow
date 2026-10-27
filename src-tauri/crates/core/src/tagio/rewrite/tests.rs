use super::*;

#[test]
fn a_failed_rewrite_leaves_the_original_exactly_as_it_was() {
    // The whole point. lofty's own rewrite truncates the file it is
    // rewriting before it writes anything back, so a body that fails
    // half way used to be the difference between an edit and a
    // ruined track.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.mp3");
    std::fs::write(&path, b"the original bytes").expect("seed");

    let outcome: Result<(), io::Error> = rewrite_via_temp(&path, |file| {
        use std::io::Write;
        // Do real damage first, then fail: a body that fails before
        // touching anything would prove nothing.
        file.set_len(0)?;
        file.write_all(b"half")?;
        Err(io::Error::other("the write gave up here"))
    });

    assert!(outcome.is_err());
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"the original bytes",
        "the original is untouched until the rename"
    );
}

#[test]
fn nothing_is_left_beside_the_users_music() {
    // A scratch file dropped in a library folder is one the scanner
    // eventually finds, so it has to go on every way out.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.mp3");
    std::fs::write(&path, b"original").expect("seed");

    let _ = rewrite_via_temp(&path, |_| -> Result<(), io::Error> {
        Err(io::Error::other("no"))
    });
    rewrite_via_temp(&path, |file| -> Result<(), io::Error> {
        use std::io::Write;
        file.set_len(0)?;
        file.write_all(b"rewritten")
    })
    .expect("rewrite");

    let left: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(left.len(), 1, "only the track remains: {left:?}");
    assert_eq!(std::fs::read(&path).expect("read"), b"rewritten");
}

#[test]
fn two_rewrites_of_one_file_take_turns() {
    // Three commands reach a tag write and each hands it to the
    // blocking pool, so two can be here on the same file at once.
    // Interleaved, they race on the rename and one edit vanishes;
    // worse, each temp file is deleted by the other's cleanup.
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("contested.mp3");
    std::fs::write(&path, b"original").expect("seed");

    let inside = Arc::new(AtomicUsize::new(0));
    let overlapped = Arc::new(AtomicUsize::new(0));

    let threads: Vec<_> = (0..4)
        .map(|i| {
            let path = path.clone();
            let inside = Arc::clone(&inside);
            let overlapped = Arc::clone(&overlapped);
            std::thread::spawn(move || {
                rewrite_via_temp(&path, |file| -> Result<(), io::Error> {
                    use std::io::Write;
                    if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                        overlapped.fetch_add(1, Ordering::SeqCst);
                    }
                    // Long enough that an unserialised pair would
                    // certainly be caught overlapping.
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    file.set_len(0)?;
                    let written = format!("written by {i}");
                    file.write_all(written.as_bytes())?;
                    inside.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("join").expect("rewrite");
    }

    assert_eq!(
        overlapped.load(Ordering::SeqCst),
        0,
        "two rewrites of the same file were inside at once"
    );
    // And the file is one writer's whole output, not a blend.
    let after = std::fs::read(&path).expect("read");
    let text = String::from_utf8(after).expect("utf8");
    assert!(text.starts_with("written by "), "got {text:?}");
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(left.len(), 1, "no scratch file survived: {left:?}");
}

#[cfg(windows)]
#[test]
fn the_read_only_attribute_lands_on_the_file_that_ends_up_in_place() {
    // The rename replaces the inode, so restoring the attribute has
    // to happen after it — on the new file, not the one that is
    // already gone.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.mp3");
    std::fs::write(&path, b"original").expect("seed");
    let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).expect("set read-only");

    rewrite_via_temp(&path, |file| -> Result<(), io::Error> {
        use std::io::Write;
        file.set_len(0)?;
        file.write_all(b"rewritten")
    })
    .expect("rewrite");

    assert_eq!(std::fs::read(&path).expect("read"), b"rewritten");
    assert!(
        std::fs::metadata(&path)
            .expect("meta")
            .permissions()
            .readonly(),
        "the attribute the user set is back on the file that replaced it"
    );
}
