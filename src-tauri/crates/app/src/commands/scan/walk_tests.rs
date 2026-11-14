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

/// An ISO-BMFF file with one track of `handler` / `codec`, enough for
/// `has_video_track` to judge.
fn mp4_with(handler: &[u8; 4], codec: &[u8; 4]) -> Vec<u8> {
    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(handler);
    hdlr.extend_from_slice(&[0u8; 12]);
    let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend(boxed(codec, &[0u8; 16]));
    let minf = boxed(b"minf", &boxed(b"stbl", &boxed(b"stsd", &stsd)));
    let mut mdia = boxed(b"hdlr", &hdlr);
    mdia.extend(minf);
    let moov = boxed(b"moov", &boxed(b"trak", &boxed(b"mdia", &mdia)));
    let mut out = boxed(b"ftyp", b"isom\0\0\0\0");
    out.extend(moov);
    out
}

/// #1073: a music video beside its song is the song's clip and leaves the
/// walk; an audio `.mp4` of the same name, and a video with no song, stay.
#[test]
fn a_music_video_beside_its_song_is_not_a_second_track() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let put = |name: &str, bytes: &[u8]| {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };
    put("A/Song.flac", b"x");
    put("A/Song.mp4", &mp4_with(b"vide", b"avc1"));
    put("A/Clips/Song.mp4", &mp4_with(b"vide", b"avc1"));
    put("B/Track.flac", b"x");
    put("B/Track.mp4", &mp4_with(b"soun", b"mp4a"));
    put("C/Lone.mp4", &mp4_with(b"vide", b"avc1"));

    let found = collect_scannable_files(root);
    let mut names: Vec<String> = found
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["A/Song.flac", "B/Track.flac", "B/Track.mp4", "C/Lone.mp4"]
    );
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
            .unwrap()
            .unwrap();
    assert_eq!(found.files.len(), 80);
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
    assert!(listed.unwrap().is_none());
}

/// A root that cannot be read is an error, never an empty list: the scan
/// sweeps whatever the list leaves out.
#[test]
fn a_missing_root_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let listed = collect_scannable_files_with(&dir.path().join("gone"), |_, _| {}, || false);
    assert!(listed.is_err());
}

/// A file where the folder was is no folder, not even when it is audio.
#[test]
fn a_root_that_is_a_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.flac");
    write(&file);
    let listed = collect_scannable_files_with(&file, |_, _| {}, || false);
    assert!(listed.is_err());
}

/// An empty folder that answers is the one case an empty list is the truth.
#[test]
fn an_empty_root_lists_nothing_and_misses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let walk = collect_scannable_files_with(dir.path(), |_, _| {}, || false)
        .unwrap()
        .unwrap();
    assert!(walk.files.is_empty());
    assert!(walk.unreadable.is_empty());
}

/// A link to the folder is the folder: a library kept behind one still scans.
#[cfg(unix)]
#[test]
fn a_root_behind_a_link_is_walked() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("real/a.flac"));
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(dir.path().join("real"), &link).unwrap();
    let walk = collect_scannable_files_with(&link, |_, _| {}, || false)
        .unwrap()
        .unwrap();
    assert_eq!(walk.files.len(), 1);
}

/// A directory the walk may not enter ends nothing, and its files are the
/// ones the sweep has to leave alone, by component and not by prefix.
#[cfg(unix)]
#[test]
fn an_unreadable_directory_is_reported_and_the_walk_goes_on() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("locked/a.flac"));
    write(&root.join("locked2/b.flac"));
    let locked = root.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions do not stop anything here.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let walk = collect_scannable_files_with(root, |_, _| {}, || false)
        .unwrap()
        .unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(walk.files.len(), 1);
    assert_eq!(walk.unreadable.as_slice(), std::slice::from_ref(&locked));
    assert!(missed(&walk.unreadable, &locked.join("a.flac")));
    assert!(!missed(&walk.unreadable, &root.join("locked2/b.flac")));
}
