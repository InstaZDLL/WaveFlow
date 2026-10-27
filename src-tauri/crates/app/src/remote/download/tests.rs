use super::*;

#[test]
fn a_track_id_never_reaches_the_path() {
    let name = file_name("../../etc/passwd", "flac");
    assert!(!name.contains('/'), "{name}");
    assert!(name.ends_with(".flac"), "{name}");
}

#[test]
fn a_hostile_extension_cannot_escape_either() {
    let name = file_name("t1", "../../sh");
    assert!(!name.contains('/'), "{name}");
    assert!(name.ends_with(".sh"), "{name}");
    assert!(file_name("t1", "").ends_with(".bin"));
}

#[test]
fn two_tracks_never_share_a_file() {
    assert_ne!(file_name("t1", "flac"), file_name("t2", "flac"));
    // Same track, same name: asking twice must land on the copy that is
    // already there rather than beside it.
    assert_eq!(file_name("t1", "flac"), file_name("t1", "flac"));
}
