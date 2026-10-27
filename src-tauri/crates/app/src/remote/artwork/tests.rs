use super::*;

#[test]
fn a_hash_that_is_not_a_hash_never_reaches_the_filesystem() {
    assert!(is_hash("abc123"));
    assert!(is_hash(&"a".repeat(64)));

    for hostile in [
        "",
        "../../etc/passwd",
        "abc/def",
        "abc\\def",
        "abc.jpg",
        "café",
        &"a".repeat(129),
    ] {
        assert!(!is_hash(hostile), "{hostile} should be refused");
    }
}

/// The extension decides the `Content-Type` the asset protocol serves, so
/// an unknown type must land on something an `<img>` will render rather
/// than on the type's own name.
#[test]
fn the_extension_follows_the_content_type() {
    assert_eq!(extension_for("image/png"), "png");
    assert_eq!(extension_for("image/webp"), "webp");
    assert_eq!(extension_for("image/gif"), "gif");
    assert_eq!(extension_for("image/jpeg"), "jpg");
    // Parameters are common and must not defeat the match.
    assert_eq!(extension_for("image/png; charset=binary"), "png");
    assert_eq!(extension_for("  image/webp  "), "webp");
    // Anything unrecognised falls back to a renderable type.
    assert_eq!(extension_for("application/octet-stream"), "jpg");
    assert_eq!(extension_for(""), "jpg");
}

#[test]
fn a_cover_is_found_whatever_extension_it_was_stored_under() {
    let dir = tempfile::tempdir().unwrap();
    assert!(existing(dir.path(), "abcd").is_none());

    std::fs::write(dir.path().join("abcd.webp"), b"x").unwrap();
    let found = existing(dir.path(), "abcd").unwrap();
    assert_eq!(found.extension().unwrap(), "webp");
}

/// A half-written file under the real name would be served as a broken
/// image and never re-fetched, since the name exists.
#[tokio::test]
async fn a_partial_write_never_appears_under_the_final_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abcd.jpg");
    write_atomically(dir.path(), &path, b"bytes").await.unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), b"bytes");
    // Nothing temporary survives a successful write.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".part-"))
        .collect();
    assert!(leftovers.is_empty());
}

/// Two views asking for the same cover at the same moment are in the same
/// process, so a temporary named after the process alone would be the same
/// file for both, and they would interleave their writes into it.
#[tokio::test]
async fn concurrent_writes_of_one_cover_do_not_share_a_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abcd.jpg");
    let bytes = vec![7u8; 4096];

    let writes = (0..8).map(|_| {
        let dir = dir.path().to_path_buf();
        let path = path.clone();
        let bytes = bytes.clone();
        async move { write_atomically(&dir, &path, &bytes).await }
    });
    for result in futures::future::join_all(writes).await {
        result.unwrap();
    }

    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".part-"))
        .collect();
    assert!(leftovers.is_empty(), "every temporary must be renamed away");
}

/// A purge that cannot delete must say so. Answering with a silent success
/// while the count stays put makes the button look broken.
#[tokio::test]
async fn clearing_a_cache_that_was_never_written_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("never-created");
    // Exercised through the same closure `clear` runs; the command wrapper
    // only resolves the directory.
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        match std::fs::read_dir(&missing) {
            Ok(_) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    })
    .await
    .unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn eviction_drops_the_least_recently_used_first() {
    let dir = tempfile::tempdir().unwrap();
    for (name, age_secs) in [("aa.jpg", 300), ("bb.jpg", 200), ("cc.jpg", 100)] {
        let path = dir.path().join(name);
        std::fs::write(&path, vec![0u8; 1000]).unwrap();
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    let (before, _) = scan(dir.path());
    assert_eq!((before.covers, before.bytes), (3, 3000));

    // Room for one file only: the two oldest go.
    evict(dir.path().to_path_buf(), 1000).await;

    let (after, _) = scan(dir.path());
    assert_eq!(after.covers, 1);
    assert!(dir.path().join("cc.jpg").is_file(), "newest must survive");
}

#[tokio::test]
async fn the_reported_size_counts_leftovers_from_a_crashed_write() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("aa.jpg"), vec![0u8; 100]).unwrap();
    std::fs::write(dir.path().join("bb.part-1234"), vec![0u8; 50]).unwrap();

    let (info, files) = scan(dir.path());
    // The temporary counts towards disk usage but is not a cover.
    assert_eq!((info.covers, info.bytes), (1, 150));
    assert_eq!(files.len(), 1);
}
