use super::*;

#[test]
fn thumbnails_land_whole_and_leave_no_partial() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("abc.png");
    image::RgbaImage::from_pixel(300, 200, image::Rgba([200, 40, 90, 255]))
        .save(&source)
        .unwrap();

    generate_thumbnails(&source, dir.path(), "abc").unwrap();

    for size in [THUMB_SMALL, THUMB_MEDIUM] {
        let thumb = image::open(thumbnail_path(dir.path(), "abc", size)).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (size, size));
    }
    let partials = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with(crate::atomic_write::PARTIAL_SUFFIX)
        })
        .count();
    assert_eq!(partials, 0);
}

/// Hashes with both thumbnails, with only the small one, with a partial
/// left by an interrupted write, and with none at all.
fn thumbnail_fixture(dir: &Path) -> Vec<&'static str> {
    std::fs::write(thumbnail_path(dir, "both", THUMB_SMALL), b"x").unwrap();
    std::fs::write(thumbnail_path(dir, "both", THUMB_MEDIUM), b"x").unwrap();
    std::fs::write(thumbnail_path(dir, "small", THUMB_SMALL), b"x").unwrap();
    std::fs::write(dir.join("both.jpg"), b"x").unwrap();
    let partial = format!(
        "{}{}",
        thumbnail_path(dir, "partial", THUMB_MEDIUM).display(),
        crate::atomic_write::PARTIAL_SUFFIX
    );
    std::fs::write(partial, b"x").unwrap();
    vec!["both", "small", "partial", "none"]
}

#[test]
fn a_listed_index_answers_like_the_per_file_probe() {
    let dir = tempfile::tempdir().unwrap();
    let hashes = thumbnail_fixture(dir.path());
    let listed = ThumbnailIndex::new(dir.path(), LIST_THRESHOLD);
    assert!(
        listed.names.is_some(),
        "a batch at the threshold lists the directory"
    );
    for hash in hashes {
        assert_eq!(
            listed.paths_for(hash),
            thumbnail_paths_for(dir.path(), hash),
            "{hash}"
        );
    }
    let (p1, p2) = listed.paths_for("both");
    assert_eq!(
        p1.as_deref(),
        Some(
            thumbnail_path(dir.path(), "both", THUMB_SMALL)
                .to_str()
                .unwrap()
        )
    );
    assert!(p2.is_some());
    assert_eq!(listed.paths_for("small").1, None);
    assert_eq!(listed.paths_for("partial"), (None, None));
}

#[test]
fn a_small_batch_asks_the_disk_per_file() {
    let dir = tempfile::tempdir().unwrap();
    thumbnail_fixture(dir.path());
    let index = ThumbnailIndex::new(dir.path(), LIST_THRESHOLD - 1);
    assert!(index.names.is_none());
    assert!(index.paths_for("both").0.is_some());
    // Written after the index was built: still found, since nothing was listed.
    std::fs::write(thumbnail_path(dir.path(), "late", THUMB_SMALL), b"x").unwrap();
    assert!(index.paths_for("late").0.is_some());
}

#[test]
fn a_missing_directory_lists_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let index = ThumbnailIndex::new(&dir.path().join("absent"), 10_000);
    assert_eq!(index.names.as_ref().map(|n| n.len()), Some(0));
    assert_eq!(index.paths_for("both"), (None, None));
}
