use super::*;

fn touch(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn never() -> bool {
    false
}

#[test]
fn a_folder_keeps_its_tree_and_lyrics_join_their_audio() {
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("Downloads").join("Album");
    touch(&album.join("01.flac"), b"a");
    touch(&album.join("CD2").join("01.mp3"), b"b");
    touch(&album.join("cover.jpg"), b"c");
    touch(&album.join("01.LRC"), b"d");
    touch(&album.join("booklet.lrc"), b"g");
    touch(&album.join("notes.pdf"), b"e");
    touch(&album.join(".waveflow").join("clip.mp4"), b"f");
    let managed = dir.path().join("Music").join("WaveFlow");

    let plan = plan(std::slice::from_ref(&album), &managed, &[], &never);
    let base = managed.join("Album");
    let mut to: Vec<_> = plan.copies.iter().map(|c| c.to.clone()).collect();
    to.sort();
    assert_eq!(
        to,
        vec![
            base.join("01.flac"),
            base.join("CD2").join("01.mp3"),
            base.join("booklet.lrc"),
            base.join("cover.jpg"),
        ]
    );
    let flac = plan
        .copies
        .iter()
        .find(|c| c.to == base.join("01.flac"))
        .unwrap();
    assert_eq!(flac.lyrics, vec![album.join("01.LRC")]);
    assert_eq!(plan.read_errors, 0);
}

#[test]
fn a_file_goes_to_the_root_with_its_lyrics() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("Downloads").join("song.mp3");
    touch(&song, b"a");
    touch(&song.with_extension("lrc"), b"b");
    touch(&dir.path().join("Downloads").join("other.mp3"), b"c");
    let managed = dir.path().join("Music").join("WaveFlow");

    let plan = plan(std::slice::from_ref(&song), &managed, &[], &never);
    assert_eq!(
        plan.copies,
        vec![Copy {
            from: song.clone(),
            to: managed.join("song.mp3"),
            lyrics: vec![song.with_extension("lrc")],
        }]
    );
}

#[test]
fn library_items_are_passed_over_and_folders_holding_one_refused() {
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("Music");
    let library = music.join("Albums");
    touch(&library.join("Album").join("01.flac"), b"a");
    touch(&music.join("Loose").join("02.flac"), b"b");
    let managed = music.join("WaveFlow");

    // The library's own album, the music folder that holds both the
    // library folder and the managed one, and a file already copied.
    touch(&managed.join("03.flac"), b"c");
    let dropped = [
        library.join("Album"),
        music.clone(),
        managed.join("03.flac"),
    ];
    let plan = plan(&dropped, &managed, std::slice::from_ref(&library), &never);
    assert!(plan.copies.is_empty());
    assert_eq!(plan.in_library, 2);
    assert_eq!(plan.refused, 1);
}

#[test]
fn a_stop_ends_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("song.mp3");
    touch(&song, b"a");
    let plan = plan(&[song], &dir.path().join("WaveFlow"), &[], &|| true);
    assert!(plan.stopped);
    assert!(plan.copies.is_empty());
}

#[test]
fn a_copy_is_not_repeated_and_a_different_file_gets_a_number() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("in").join("song.mp3");
    touch(&from, b"abc");
    let to = dir.path().join("out").join("song.mp3");

    assert_eq!(copy_one(&from, &to).unwrap(), Copied::New(to.clone()));
    assert_eq!(
        copy_one(&from, &to).unwrap(),
        Copied::AlreadyThere(to.clone())
    );

    let other = dir.path().join("in2").join("song.mp3");
    touch(&other, b"abcd");
    let numbered = dir.path().join("out").join("song (2).mp3");
    assert_eq!(
        copy_one(&other, &to).unwrap(),
        Copied::New(numbered.clone())
    );
    assert_eq!(std::fs::read(numbered).unwrap(), b"abcd");
    // Nothing half-written is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("out"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".waveflow-part"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn lyrics_take_the_name_their_numbered_audio_got() {
    let dir = tempfile::tempdir().unwrap();
    let managed = dir.path().join("WaveFlow");
    // Another song.mp3 already in the managed folder, with its lyrics.
    touch(&managed.join("song.mp3"), b"other");
    touch(&managed.join("song.lrc"), b"other lyrics");
    let song = dir.path().join("in").join("song.mp3");
    touch(&song, b"mine");
    touch(&song.with_extension("lrc"), b"my lyrics");

    let plan = plan(std::slice::from_ref(&song), &managed, &[], &never);
    let mut summary = DropImportSummary::default();
    for copy in &plan.copies {
        apply(copy, &mut summary);
    }
    assert_eq!(summary.copied, 2);
    assert_eq!(
        std::fs::read(managed.join("song (2).mp3")).unwrap(),
        b"mine"
    );
    assert_eq!(
        std::fs::read(managed.join("song (2).lrc")).unwrap(),
        b"my lyrics"
    );
    // The other song's lyrics are untouched.
    assert_eq!(
        std::fs::read(managed.join("song.lrc")).unwrap(),
        b"other lyrics"
    );
}
