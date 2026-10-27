use super::{is_plain_absolute, video_mime};
use std::path::Path;

#[test]
fn only_plain_absolute_paths_are_accepted() {
    assert!(is_plain_absolute(Path::new("/home/a/canvas/x.mp4")));
    assert!(!is_plain_absolute(Path::new("/home/a/../../etc/x.mp4")));
    assert!(!is_plain_absolute(Path::new("relative/x.mp4")));
}

#[test]
fn only_video_containers_are_served() {
    assert_eq!(video_mime(Path::new("/a/clip.MP4")), Some("video/mp4"));
    assert_eq!(
        video_mime(Path::new("/a/clip.mov")),
        Some("video/quicktime")
    );
    assert_eq!(video_mime(Path::new("/a/cover.jpg")), None);
    assert_eq!(video_mime(Path::new("/a/noext")), None);
}
