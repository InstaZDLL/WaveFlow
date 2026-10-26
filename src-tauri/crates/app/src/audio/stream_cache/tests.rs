use super::*;

#[test]
fn the_key_separates_what_produces_different_bytes() {
    let raw = file_name("t1", "raw", 0, "flac");
    let mp3 = file_name("t1", "mp3", 192, "mp3");
    let quieter = file_name("t1", "mp3", 128, "mp3");
    let other = file_name("t2", "raw", 0, "flac");
    assert_ne!(raw, mp3, "format changes the bytes");
    assert_ne!(mp3, quieter, "bitrate changes the bytes");
    assert_ne!(raw, other, "track changes the bytes");
    assert!(raw.ends_with(".flac"), "extension stays legible: {raw}");
}

#[test]
fn an_extension_from_the_server_cannot_escape_the_directory() {
    let name = file_name("t1", "raw", 0, "../../etc");
    assert!(!name.contains('/'), "{name}");
    assert!(
        !name.contains('.') || name.matches('.').count() == 1,
        "{name}"
    );
    let empty = file_name("t1", "raw", 0, "");
    assert!(empty.ends_with(".bin"), "{empty}");
}

#[test]
fn only_a_fully_covered_body_is_published() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = file_name("t1", "raw", 0, "flac");
    let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
    writer.write_at(0, &[0u8; 4]);
    assert!(
        lookup(dir.path(), &name).is_none(),
        "a partial body must not be visible"
    );
    writer.write_at(4, &[0u8; 6]);
    assert!(
        lookup(dir.path(), &name).is_some(),
        "a complete body is published"
    );
}

#[test]
fn a_hole_left_by_a_seek_keeps_the_entry_unpublished() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = file_name("t2", "raw", 0, "flac");
    let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
    // Probe reads the head, the decoder then seeks past the middle: the
    // skipped bytes were never fetched, so the file must not be offered.
    writer.write_at(0, &[0u8; 2]);
    writer.write_at(6, &[0u8; 4]);
    assert!(lookup(dir.path(), &name).is_none());
    // Filling the hole completes it, in whatever order it arrives.
    writer.write_at(2, &[0u8; 4]);
    assert!(lookup(dir.path(), &name).is_some());
}

#[test]
fn overlapping_reads_do_not_count_twice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = file_name("t3", "raw", 0, "flac");
    let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
    // A re-read of already-covered bytes must not make a short body look
    // whole — the naive "sum the lengths" tally would publish here.
    writer.write_at(0, &[0u8; 6]);
    writer.write_at(0, &[0u8; 6]);
    assert!(lookup(dir.path(), &name).is_none());
}

#[test]
fn a_body_longer_than_declared_is_refused_rather_than_truncated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = file_name("t4", "raw", 0, "flac");
    let mut writer = CacheWriter::create(dir.path(), &name, 4).expect("writer");
    writer.write_at(0, &[0u8; 8]);
    assert!(lookup(dir.path(), &name).is_none());
}

/// Linux only, and the exclusions are for two different reasons.
/// Windows path components are UTF-16 and cannot carry the byte
/// sequences this guards against at all. macOS can express them but
/// APFS refuses to store them: `create_dir_all` fails outright, so
/// the test cannot even set its scene there. `cfg(unix)` covered both
/// Unixes and was red on macOS from the day it landed — invisibly,
/// since no CI job builds this project on a Mac.
///
/// The code under test still matters on macOS: a path can be handed
/// to us from outside the filesystem's own naming rules.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn a_parent_directory_that_is_not_utf8_does_not_hide_a_working_file() {
    use std::os::unix::ffi::OsStrExt;

    let root = tempfile::tempdir().expect("tempdir");
    // A profile path with one stray byte in it — legal on Unix, and
    // enough to make `Path::to_str` on the full path return `None`.
    let dir = root.path().join(std::ffi::OsStr::from_bytes(b"caf\xff"));
    fs::create_dir_all(&dir).expect("non-utf8 dir");

    let name = file_name("t8", "raw", 0, "flac");
    let mut writer = CacheWriter::create(&dir, &name, 10).expect("writer");
    writer.write_at(0, &[0u8; 4]);

    assert_eq!(
        info(&dir),
        (0, 0),
        "a working file stays invisible however its parents are encoded"
    );
    // And the sweep must still refuse to touch it.
    let part = fs::read_dir(&dir)
        .expect("read_dir")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| is_working_file(path));
    assert!(
        part.is_some(),
        "the working file is still recognised as one"
    );
}

#[test]
fn an_extension_cannot_impersonate_a_working_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The one extension that would collide with a naive `.part` marker.
    // It is far-fetched as an audio suffix and entirely under the
    // server's control, which is reason enough not to let it decide
    // whether one of our files is visible.
    let name = file_name("t7", "raw", 0, "part");
    let mut writer = CacheWriter::create(dir.path(), &name, 4).expect("writer");
    writer.write_at(0, &[0u8; 4]);
    assert!(
        lookup(dir.path(), &name).is_some(),
        "a published entry is playable whatever its extension"
    );
    assert_eq!(
        info(dir.path()),
        (4, 1),
        "and it is counted, not mistaken for a file still being written"
    );
    // Evictable too: excluded from the sweep, it would outlive the budget
    // forever.
    assert!(!is_working_file(&dir.path().join(&name)));
}

#[test]
fn a_body_written_almost_to_the_end_is_not_reported_as_cached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = file_name("t6", "raw", 0, "flac");
    let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
    // One byte short: the working file is on disk and nearly the full
    // size, which is exactly when counting it would be most misleading.
    writer.write_at(0, &[0u8; 9]);
    assert!(lookup(dir.path(), &name).is_none(), "still not playable");
    assert_eq!(
        info(dir.path()),
        (0, 0),
        "a working file is disk in flight, not a cached track"
    );
    // And it must not be evicted out from under its own writer, which
    // would spend a download to reclaim bytes already about to be freed.
    writer.write_at(9, &[0u8; 1]);
    assert!(lookup(dir.path(), &name).is_some());
    assert_eq!(info(dir.path()).1, 1);
}

#[test]
fn dropping_an_incomplete_writer_leaves_nothing_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let name = file_name("t5", "raw", 0, "flac");
    {
        let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
        writer.write_at(0, &[0u8; 3]);
    }
    let (bytes, count) = info(dir.path());
    assert_eq!((bytes, count), (0, 0), "no .part file may survive");
}
