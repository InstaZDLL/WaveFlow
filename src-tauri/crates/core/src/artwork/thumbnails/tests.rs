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
