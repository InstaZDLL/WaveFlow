use super::*;

#[test]
fn a_title_keeps_what_titles_are_made_of() {
    assert_eq!(
        sanitize_component("Don't Stop (Live)", "x"),
        "Don't Stop (Live)"
    );
    assert_eq!(sanitize_component("Björk — Jóga", "x"), "Björk — Jóga");
}

#[test]
fn a_component_never_escapes_its_directory() {
    assert_eq!(sanitize_component("../../etc", "x"), ".._.._etc");
    assert_eq!(sanitize_component("a/b\\c", "x"), "a_b_c");
    assert_eq!(sanitize_component("..", "fallback"), "fallback");
    assert_eq!(sanitize_component("", "fallback"), "fallback");
    assert_eq!(sanitize_component("   ", "fallback"), "fallback");
}

#[test]
fn windows_refuses_these_and_so_do_we() {
    assert_eq!(sanitize_component("trailing.", "x"), "trailing");
    assert_eq!(sanitize_component("trailing ", "x"), "trailing");
    assert_eq!(sanitize_component("CON", "fallback"), "fallback");
    assert_eq!(sanitize_component("nul.txt", "fallback"), "fallback");
    assert_eq!(sanitize_component("a:b?c*d", "x"), "a_b_c_d");
}

#[test]
fn a_long_title_is_cut_on_a_character_not_a_byte() {
    let long = "é".repeat(200);
    let cut = sanitize_component(&long, "x");
    assert_eq!(cut.chars().count(), 60);
}

#[test]
fn a_hostile_suffix_cannot_reach_the_path() {
    assert_eq!(extension_for(Some("../../sh")), "sh");
    assert_eq!(extension_for(Some(".FLAC")), "flac");
    assert_eq!(extension_for(None), "");
}

#[test]
fn a_stem_sorts_the_way_the_album_plays() {
    let mut meta = RemoteTrackMeta {
        title: "Song".into(),
        artist: None,
        album: None,
        track_no: Some(3),
        disc_no: Some(1),
        suffix: None,
        size: None,
        full_hash: None,
    };
    assert_eq!(stem(&meta), "03 - Song");
    meta.disc_no = Some(2);
    assert_eq!(stem(&meta), "2-03 - Song");
    meta.track_no = None;
    assert_eq!(stem(&meta), "Song");
}

#[test]
fn a_name_is_claimed_not_merely_checked() {
    let dir = std::env::temp_dir().join(format!(
        "waveflow-import-test-{}",
        blake3::hash(format!("{:?}", std::time::SystemTime::now()).as_bytes()).to_hex()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Claiming creates the file, so a second claim must move on without
    // the caller having written anything in between — that gap is the
    // race an `exists()` check leaves open.
    let first = reserve_path(&dir, "01 - Song", "flac").unwrap();
    assert!(first.ends_with("01 - Song.flac"));
    assert!(first.is_file());
    let second = reserve_path(&dir, "01 - Song", "flac").unwrap();
    assert!(second.ends_with("01 - Song (2).flac"), "{second:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_working_file_carries_no_audio_extension() {
    // The invariant the scanner enforces for us: what is incomplete must
    // not be indexable. `is_scannable_audio` filters on extension, so the
    // working name has to fail that filter.
    let name = format!("{}.deadbeef.waveflow-part", blake3::hash(b"t1").to_hex());
    assert!(!waveflow_core::scanner::is_scannable_audio(Path::new(
        &name
    )));
}
