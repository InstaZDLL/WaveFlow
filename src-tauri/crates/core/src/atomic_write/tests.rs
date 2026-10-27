use super::*;

/// Partials, and a Windows replace's backups, left in `dir`.
fn partials(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.ends_with(PARTIAL_SUFFIX) || name.ends_with(REPLACED_SUFFIX)
        })
        .count()
}

#[test]
fn the_partial_sits_beside_the_target_and_keeps_its_extension_last() {
    assert_eq!(
        partial_path(Path::new("/backups/Main-2026.waveflow"), "7-1"),
        Path::new("/backups/Main-2026.7-1.waveflow.partial")
    );
    assert_eq!(
        partial_path(Path::new("/exports/list"), "7-2"),
        Path::new("/exports/list.7-2.partial")
    );
}

/// A Windows replace's backup must not be swept as an unfinished
/// write: it can hold the only copy of the old file.
#[test]
fn the_replaced_backup_is_not_a_partial() {
    let backup = sibling_path(
        Path::new("/backups/Main-2026.waveflow"),
        "7-3",
        REPLACED_SUFFIX,
    );
    assert_eq!(
        backup,
        Path::new("/backups/Main-2026.7-3.waveflow.replaced")
    );
    assert!(!backup.to_string_lossy().ends_with(PARTIAL_SUFFIX));
}

#[test]
fn a_partial_is_recognised_as_its_target_s_and_no_other_s() {
    let target = Path::new("/exports/Main.waveflow");
    let own = partial_path(target, "4242-7");
    assert!(is_partial_of(target, own.file_name().unwrap()));

    let bare = Path::new("/exports/list");
    let own = partial_path(bare, "1-0");
    assert!(is_partial_of(bare, own.file_name().unwrap()));

    for other in [
        "Main.waveflow",
        "Main.old.4242-7.waveflow.partial",
        "Main.4242-7.waveflow.replaced",
        "Main.notes-1.waveflow.partial",
        "Main.4242-.waveflow.partial",
        "Main.4242-7.m3u8.partial",
    ] {
        assert!(!is_partial_of(target, OsStr::new(other)), "{other}");
    }
}

/// A Linux file name need not be UTF-8; its partials are still found.
#[cfg(unix)]
#[test]
fn a_partial_of_a_name_that_is_not_utf8_is_recognised() {
    use std::os::unix::ffi::OsStrExt;
    let target = Path::new("/exports").join(OsStr::from_bytes(b"Caf\xe9.waveflow"));
    let own = partial_path(&target, "12-0");
    assert!(is_partial_of(&target, own.file_name().unwrap()));
    assert!(!is_partial_of(
        &target,
        OsStr::from_bytes(b"Caf\xe9.old.12-0.waveflow.partial")
    ));
}

/// An export cut short (the app closed halfway) never reaches its
/// cleanup; the next write to the same file removes what it left,
/// and nothing that may still be in use.
#[test]
fn the_next_write_removes_a_stale_partial_of_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("Main.waveflow");
    let old = SystemTime::now() - STALE_PARTIAL_AGE - Duration::from_secs(60);
    let leave = |path: &Path, modified: SystemTime| {
        std::fs::File::create(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    };
    let stale = partial_path(&target, "1-0");
    leave(&stale, old);
    let fresh = partial_path(&target, "1-1");
    leave(&fresh, SystemTime::now());
    let foreign = partial_path(&dir.path().join("Other.waveflow"), "1-0");
    leave(&foreign, old);

    write_bytes(&target, b"new").unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(foreign.exists());
}

#[test]
fn every_write_gets_its_own_partial() {
    assert_ne!(next_token(), next_token());
}

#[test]
fn a_complete_write_replaces_the_target_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("list.m3u8");
    std::fs::write(&target, b"old").unwrap();

    write_bytes(&target, b"new").unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert_eq!(partials(dir.path()), 0);
}

/// The case this exists for: a write that fails halfway must not
/// have truncated the copy the user already had.
#[test]
fn a_failed_write_keeps_the_previous_content() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("archive.waveflow");
    std::fs::write(&target, b"last week's archive").unwrap();

    let result: io::Result<()> = write_with(&target, |file| {
        file.write_all(b"half of a new one")?;
        Err(io::Error::other("disk full"))
    });

    assert!(result.is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"last week's archive");
    assert_eq!(partials(dir.path()), 0);
}

/// And with nothing there before, a failure leaves nothing at all
/// -- not an archive-shaped file a listing would take for a backup.
#[test]
fn a_failed_first_write_leaves_no_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("archive.waveflow");

    let result: io::Result<()> = write_with(&target, |file| {
        file.write_all(b"partial")?;
        Err(io::Error::other("cancelled"))
    });

    assert!(result.is_err());
    assert!(!target.exists());
    assert_eq!(partials(dir.path()), 0);
}

/// A partial someone else holds is neither truncated nor removed: it
/// may be another write in progress.
#[test]
fn another_partial_of_the_same_target_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("list.m3u8");
    let other = partial_path(&target, "1-999");
    std::fs::write(&other, b"someone else's write").unwrap();

    write_bytes(&target, b"fresh").unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"fresh");
    assert_eq!(std::fs::read(&other).unwrap(), b"someone else's write");
}

/// A replaced file keeps the permissions it had, so a private archive
/// does not come back readable by everyone.
#[cfg(unix)]
#[test]
fn a_replaced_file_keeps_its_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("private.waveflow");
    std::fs::write(&target, b"old").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

    write_bytes(&target, b"new").unwrap();

    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_missing_parent_directory_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("new").join("folder").join("list.m3u8");

    write_bytes(&target, b"x").unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"x");
}

#[test]
fn a_cached_file_is_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("artwork").join("abc.jpg");
    assert!(store_content_addressed(&target, b"cover").unwrap());
    assert_eq!(std::fs::read(&target).unwrap(), b"cover");
    assert!(!store_content_addressed(&target, b"cover").unwrap());
    assert_eq!(partials(target.parent().unwrap()), 0);
}

/// The bug the length check is for: a cover cut short under its
/// final name used to be kept for good, since the name existed.
#[test]
fn a_truncated_cached_file_is_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("abc.jpg");
    std::fs::write(&target, b"cov").unwrap();
    assert!(store_content_addressed(&target, b"cover").unwrap());
    assert_eq!(std::fs::read(&target).unwrap(), b"cover");

    std::fs::write(&target, b"").unwrap();
    assert!(store_content_addressed(&target, b"cover").unwrap());
    assert_eq!(std::fs::read(&target).unwrap(), b"cover");
    assert_eq!(partials(dir.path()), 0);
}

#[test]
fn a_cached_write_that_fails_leaves_no_partial() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where the file would go: the rename fails on
    // every platform, and nothing of the attempt stays behind.
    let target = dir.path().join("abc.jpg");
    std::fs::create_dir(&target).unwrap();
    assert!(store_content_addressed(&target, b"cover").is_err());
    assert_eq!(partials(dir.path()), 0);
}
