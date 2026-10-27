use super::*;

#[test]
fn a_folder_expands_to_its_audio_files_in_name_order() {
    let dir = tempfile::tempdir().unwrap();
    let disc = dir.path().join("CD2");
    std::fs::create_dir(&disc).unwrap();
    for name in ["b.flac", "a.mp3", "notes.txt", "cover.jpg"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    std::fs::write(disc.join("c.flac"), b"x").unwrap();
    let ours = dir.path().join(".waveflow");
    std::fs::create_dir(&ours).unwrap();
    std::fs::write(ours.join("clip.mp4"), b"x").unwrap();

    let files = expand(&[dir.path().to_path_buf()]);

    let names: Vec<_> = files
        .iter()
        .map(|p| {
            p.strip_prefix(dir.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(names, ["CD2/c.flac", "a.mp3", "b.flac"]);
}

/// A link to an audio file plays as the file. Unix only: creating a
/// link on Windows needs a privilege the CI runner may not have.
#[cfg(unix)]
#[test]
fn a_link_to_an_audio_file_counts_as_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("elsewhere.flac");
    std::fs::write(&target, b"x").unwrap();
    let folder = dir.path().join("album");
    std::fs::create_dir(&folder).unwrap();
    std::os::unix::fs::symlink(&target, folder.join("linked.flac")).unwrap();

    assert_eq!(
        expand(std::slice::from_ref(&folder)),
        [folder.join("linked.flac")]
    );
}

#[test]
fn files_keep_their_order_and_other_files_are_passed_over() {
    let dir = tempfile::tempdir().unwrap();
    let (b, a, txt) = (
        dir.path().join("b.flac"),
        dir.path().join("a.flac"),
        dir.path().join("readme.txt"),
    );
    for path in [&b, &a, &txt] {
        std::fs::write(path, b"x").unwrap();
    }
    let missing = dir.path().join("gone.flac");

    assert_eq!(expand(&[b.clone(), txt, missing, a.clone()]), [b, a]);
}

#[test]
fn a_handoff_keeps_its_place_when_files_are_gone() {
    let dir = tempfile::tempdir().unwrap();
    let [a, b, c, d] = ["a.flac", "b.flac", "c.flac", "d.flac"].map(|n| dir.path().join(n));
    for path in [&a, &c, &d] {
        std::fs::write(path, b"x").unwrap();
    }
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, b"x").unwrap();
    let queue = [a.clone(), b.clone(), notes, c.clone(), d.clone()];

    // The current file is still there: it keeps its place.
    assert_eq!(
        handed_over_queue(&queue, 3),
        Some((vec![a.clone(), c.clone(), d.clone()], 1))
    );
    // It is gone: the next one that is there.
    assert_eq!(
        handed_over_queue(&queue, 1),
        Some((vec![a.clone(), c.clone(), d.clone()], 1))
    );
    // Out of range: the last one.
    assert_eq!(handed_over_queue(&queue, 9), Some((vec![a, c, d], 2)));
    assert_eq!(handed_over_queue(&[b], 0), None);
}

#[test]
fn a_file_without_tags_reads_as_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Track 07.flac");
    std::fs::write(&path, b"not really a flac").unwrap();

    let facts = read_facts(&path, dir.path());

    assert_eq!(facts.title, "Track 07");
    assert_eq!(facts.artist, None);
    assert_eq!(facts.cover, None);
}
