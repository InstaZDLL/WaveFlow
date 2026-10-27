use super::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

fn tree(root: &Path) {
    for name in ["a.jpg", "b.jpg", "sub/c.jpg", "sub/deeper/d.jpg"] {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }
}

#[test]
fn copy_tree_counts_what_it_copies_and_matches_the_count() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    tree(src.path());
    let seen = std::sync::atomic::AtomicU64::new(0);
    let copied = copy_tree(
        src.path(),
        &dst.path().join("out"),
        &AtomicBool::new(false),
        &|| {
            seen.fetch_add(1, Ordering::Relaxed);
        },
    )
    .unwrap();
    assert_eq!(copied, 4);
    assert_eq!(seen.load(Ordering::Relaxed), 4);
    // The total a move's progress is measured against is the same count.
    assert_eq!(count_files(src.path()), 4);
    assert!(dst.path().join("out/sub/deeper/d.jpg").is_file());
}

#[test]
fn copy_tree_stops_between_two_files_when_asked() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    tree(src.path());
    let stop = AtomicBool::new(false);
    let seen = std::sync::atomic::AtomicU64::new(0);
    let result = copy_tree(src.path(), &dst.path().join("out"), &stop, &|| {
        // Asked to stop after the first file.
        seen.fetch_add(1, Ordering::Relaxed);
        stop.store(true, Ordering::Release);
    });
    assert!(matches!(result, Err(CopyStop::Cancelled)));
    assert_eq!(seen.load(Ordering::Relaxed), 1);
}
