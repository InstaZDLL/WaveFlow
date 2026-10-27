use super::{verify_groups_full_content, DuplicateTrack};

fn track(path: String, added_at: i64) -> DuplicateTrack {
    DuplicateTrack {
        id: added_at,
        title: "t".into(),
        artist_name: None,
        album_title: None,
        file_path: path,
        file_size: 0,
        bitrate: None,
        sample_rate: None,
        duration_ms: 0,
        added_at,
    }
}

#[test]
fn groups_identical_files_by_content_not_stored_hash() {
    // Two byte-identical files + one different. In a mixed library
    // the identical pair could carry a legacy full hash on one row
    // and a new partial hash on the other; verify reads the bytes,
    // so they still group. `DuplicateTrack` carries no stored hash —
    // that's the point: identity is content-derived here.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    let c = dir.path().join("c.bin");
    std::fs::write(&a, b"same bytes").unwrap();
    std::fs::write(&b, b"same bytes").unwrap();
    std::fs::write(&c, b"different bytes").unwrap();

    let groups = verify_groups_full_content(vec![
        track(a.to_string_lossy().into_owned(), 1),
        track(b.to_string_lossy().into_owned(), 2),
        track(c.to_string_lossy().into_owned(), 3),
    ]);

    assert_eq!(groups.len(), 1, "only the identical pair forms a group");
    let mut ids: Vec<i64> = groups[0].tracks.iter().map(|t| t.id).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 2]);
}

/// The checks made right before a move: each refusal keeps the file
/// where it is, and only a copy with an identical kept twin goes.
#[test]
fn a_copy_moves_only_while_its_kept_twin_still_matches() {
    use super::{trash_one, TrashJob, TrashRefusal};
    use crate::audio::state::SharedPlayback;

    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    };
    let job = |id: i64, file_path: String, keep_path: Option<String>| TrashJob {
        id,
        library_id: 1,
        file_path,
        keep_path,
    };
    let shared = SharedPlayback::new();

    // The kept copy's row is gone.
    let lone = write("lone.mp3", b"same");
    assert!(matches!(
        trash_one(&job(1, lone.clone(), None), &shared),
        Err(TrashRefusal::KeptCopyGone)
    ));
    // The kept copy's file is gone.
    let missing = dir
        .path()
        .join("missing.mp3")
        .to_string_lossy()
        .into_owned();
    assert!(matches!(
        trash_one(&job(1, lone.clone(), Some(missing)), &shared),
        Err(TrashRefusal::KeptCopyGone)
    ));
    // The kept copy changed since the scan.
    let changed = write("changed.mp3", b"retagged");
    assert!(matches!(
        trash_one(&job(1, lone.clone(), Some(changed)), &shared),
        Err(TrashRefusal::KeptCopyGone)
    ));
    // The "copy" is the kept file itself.
    assert!(matches!(
        trash_one(&job(1, lone.clone(), Some(lone.clone())), &shared),
        Err(TrashRefusal::KeptCopyGone)
    ));
    // It started playing after the click.
    let twin = write("twin.mp3", b"same");
    shared
        .current_track_id
        .store(7, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        trash_one(&job(7, lone.clone(), Some(twin.clone())), &shared),
        Err(TrashRefusal::Playing)
    ));
    assert!(std::path::Path::new(&lone).exists());

    // A copy no longer on disk keeps its row.
    let vanished = dir
        .path()
        .join("vanished.mp3")
        .to_string_lossy()
        .into_owned();
    assert!(matches!(
        trash_one(&job(1, vanished, Some(twin.clone())), &shared),
        Err(TrashRefusal::Missing)
    ));

    // An identical twin still there: the copy goes.
    assert!(trash_one(&job(1, lone.clone(), Some(twin.clone())), &shared).is_ok());
    assert!(!std::path::Path::new(&lone).exists());
    assert!(std::path::Path::new(&twin).exists());
}

/// A group of three: the kept copy changes between the first move and
/// the second, and the second copy stays.
#[test]
fn the_kept_copy_is_read_again_before_every_move() {
    use super::{trash_one, TrashJob, TrashRefusal};
    use crate::audio::state::SharedPlayback;

    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: &[u8]| {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    };
    let kept = write("kept.mp3", b"same");
    let first = write("first.mp3", b"same");
    let second = write("second.mp3", b"same");
    let job = |id: i64, file_path: String| TrashJob {
        id,
        library_id: 1,
        file_path,
        keep_path: Some(kept.clone()),
    };
    let shared = SharedPlayback::new();

    assert!(trash_one(&job(1, first.clone()), &shared).is_ok());
    std::fs::write(&kept, b"retagged in between").unwrap();
    assert!(matches!(
        trash_one(&job(2, second.clone()), &shared),
        Err(TrashRefusal::KeptCopyGone)
    ));
    assert!(std::path::Path::new(&second).exists());
}

#[test]
fn unreadable_files_are_excluded() {
    let groups = verify_groups_full_content(vec![
        track("/no/such/file/x.bin".into(), 1),
        track("/no/such/file/y.bin".into(), 2),
    ]);
    assert!(groups.is_empty());
}
