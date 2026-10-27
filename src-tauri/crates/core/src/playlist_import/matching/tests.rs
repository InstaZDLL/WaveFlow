use super::*;

fn track(id: i64, title: &str, artists: &[&str], duration_ms: i64) -> Candidate {
    Candidate {
        track_id: id,
        title: title.into(),
        artists: artists.iter().map(|a| a.to_string()).collect(),
        duration_ms,
        ..Candidate::default()
    }
}

fn wanted<'a>(title: &'a str, artist: Option<&'a str>, duration_ms: Option<i64>) -> Wanted<'a> {
    Wanted {
        title,
        artist,
        duration_ms,
        ..Wanted::default()
    }
}

#[test]
fn the_isrc_wins_over_a_title_that_disagrees() {
    let mut tagged = track(2, "Totally different title", &["X"], 200_000);
    tagged.isrc = Some("GBAYE0601690".into());
    let index = LibraryIndex::new(vec![
        track(1, "Hey Jude", &["The Beatles"], 431_000),
        tagged,
    ]);
    let mut w = wanted("Hey Jude", Some("The Beatles"), Some(431_000));
    w.isrc = Some("GBAYE0601690");
    assert_eq!(index.find(&w), Some(2));
}

#[test]
fn among_tracks_sharing_an_isrc_the_closest_length_wins() {
    let mut single = track(1, "A", &["B"], 180_000);
    single.isrc = Some("USRC17607839".into());
    let mut album = track(2, "A", &["B"], 240_000);
    album.isrc = Some("USRC17607839".into());
    let index = LibraryIndex::new(vec![single, album]);
    let mut w = wanted("A", None, Some(239_000));
    w.isrc = Some("USRC17607839");
    assert_eq!(index.find(&w), Some(2));
}

#[test]
fn title_and_artist_match_across_case_accents_and_credits() {
    let index = LibraryIndex::new(vec![
        track(1, "Jóga", &["Björk"], 305_000),
        track(2, "Get Lucky", &["Daft Punk", "Pharrell Williams"], 369_000),
    ]);
    assert_eq!(
        index.find(&wanted("joga", Some("Bjork"), Some(304_000))),
        Some(1)
    );
    assert_eq!(
        index.find(&wanted(
            "Get Lucky",
            Some("Daft Punk feat. Pharrell Williams"),
            None
        )),
        Some(2)
    );
    // The wrong artist is not the same song.
    assert_eq!(
        index.find(&wanted("Get Lucky", Some("Someone Else"), None)),
        None
    );
}

#[test]
fn a_length_too_far_off_is_another_recording() {
    let index = LibraryIndex::new(vec![track(1, "Song", &["Band"], 200_000)]);
    assert_eq!(
        index.find(&wanted("Song", Some("Band"), Some(260_000))),
        None
    );
    assert_eq!(
        index.find(&wanted("Song", Some("Band"), Some(202_000))),
        Some(1)
    );
}

#[test]
fn a_version_suffix_matches_only_on_the_same_length() {
    let index = LibraryIndex::new(vec![track(1, "Song", &["Band"], 200_000)]);
    assert_eq!(
        index.find(&wanted(
            "Song (Remastered 2011)",
            Some("Band"),
            Some(201_000)
        )),
        Some(1)
    );
    assert_eq!(
        index.find(&wanted("Song - Live", Some("Band"), Some(320_000))),
        None
    );
    // Without a length to check, a suffix is not enough.
    assert_eq!(
        index.find(&wanted("Song - Radio Edit", Some("Band"), None)),
        None
    );
}

#[test]
fn an_entry_without_artist_or_isrc_is_never_matched_by_title() {
    let index = LibraryIndex::new(vec![track(1, "Intro", &["Anyone"], 60_000)]);
    assert_eq!(index.find(&wanted("Intro", None, Some(60_000))), None);
}

#[test]
fn a_path_or_file_name_wins_first() {
    let mut by_path = track(1, "Unrelated", &["Z"], 1);
    by_path.path_key = Some("c:\\music\\a.flac".into());
    by_path.file_name = Some("a.flac".into());
    let index = LibraryIndex::new(vec![by_path, track(2, "Song", &["Band"], 200_000)]);
    let mut w = wanted("Song", Some("Band"), None);
    w.path_key = Some("c:\\music\\a.flac");
    assert_eq!(index.find(&w), Some(1));
    // Moved: the file name finds it, when the artist agrees or the
    // entry names none.
    let mut w = wanted("Song", Some("Z"), None);
    w.path_key = Some("d:\\moved\\a.flac");
    w.file_name = Some("a.flac");
    assert_eq!(index.find(&w), Some(1));
    w.artist = None;
    assert_eq!(index.find(&w), Some(1));
    // Another artist's file of the same name is not it: the title
    // match takes over.
    w.artist = Some("Band");
    assert_eq!(index.find(&w), Some(2));
}

#[test]
fn a_file_name_two_files_share_needs_an_artist() {
    let mut first = track(1, "Intro", &["X"], 60_000);
    first.file_name = Some("01.mp3".into());
    let mut second = track(2, "Intro", &["Y"], 60_000);
    second.file_name = Some("01.mp3".into());
    let index = LibraryIndex::new(vec![first, second]);
    let mut w = wanted("01", None, None);
    w.file_name = Some("01.mp3");
    assert_eq!(index.find(&w), None);
    w.artist = Some("Y");
    assert_eq!(index.find(&w), Some(2));
}

#[test]
fn base_titles() {
    assert_eq!(base_title("Song (Live) [2011 Remaster]"), "Song");
    assert_eq!(base_title("Song - Radio Edit"), "Song");
    assert_eq!(base_title("(Untitled)"), "(Untitled)");
    assert_eq!(base_title("Plain"), "Plain");
}
