use super::*;
use std::path::{Path, PathBuf};

fn write(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"x").unwrap();
}

/// The whole point of the reserved directory: WaveFlow's own clips
/// live in the library folder without becoming tracks in it. `mp4` is
/// an audio extension, and it is taken on sight, so nothing but this
/// exclusion stands between a Canvas clip and the track list.
#[test]
fn our_own_media_is_not_indexed_as_music() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("Artist/Album/01.flac"));
    write(&root.join(".waveflow/canvas/clip.mp4"));
    write(&root.join("Artist/.waveflow/motion/cover.mp4"));
    // A real album video the user put there themselves is still theirs.
    write(&root.join("Artist/Album/bonus.mp4"));

    let found = collect_scannable_files(root);
    let names: Vec<String> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    assert!(names.contains(&"01.flac".to_string()), "{names:?}");
    assert!(names.contains(&"bonus.mp4".to_string()), "{names:?}");
    assert!(!names.contains(&"clip.mp4".to_string()), "{names:?}");
    assert!(!names.contains(&"cover.mp4".to_string()), "{names:?}");
}

/// A library folder the user happens to have named `.waveflow` is
/// still their library. Pruning the root would leave them with an
/// empty library and nothing on screen to explain it.
#[test]
fn a_library_named_like_ours_still_scans() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".waveflow");
    write(&root.join("Artist/01.flac"));

    let found = collect_scannable_files(&root);
    assert_eq!(found.len(), 1, "{found:?}");
}

/// Listing reports as it walks: files found so far and the directory
/// it is in, so a slow share no longer looks like a frozen scan.
#[test]
fn listing_reports_what_it_has_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for i in 0..80 {
        write(&root.join(format!("Album {}/{i:02}.flac", i / 10)));
    }

    let mut reports: Vec<(usize, PathBuf)> = Vec::new();
    let found =
        collect_scannable_files_with(root, |n, d| reports.push((n, d.to_path_buf())), || false)
            .unwrap();
    assert_eq!(found.len(), 80);
    assert!(reports.len() > 1, "{reports:?}");
    assert!(
        reports.windows(2).all(|w| w[0].0 <= w[1].0),
        "the count only grows: {reports:?}"
    );
    assert!(reports.iter().all(|(_, d)| d.starts_with(root)));
}

/// A stop while listing returns nothing at all, never the part of the
/// folder walked so far: the scan would sweep every track the list had
/// not reached as a deleted file.
#[test]
fn a_stopped_listing_returns_no_partial_list() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for i in 0..80 {
        write(&root.join(format!("{i:02}.flac")));
    }

    let asked = std::cell::Cell::new(0);
    let listed = collect_scannable_files_with(
        root,
        |_, _| {},
        || {
            asked.set(asked.get() + 1);
            asked.get() > 1
        },
    );
    assert!(listed.is_none());
}
