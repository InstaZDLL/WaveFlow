use super::*;
use std::path::{Path, PathBuf};
use waveflow_core::scanner::extra_tags::COMPOSER_KEY;

fn copy_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tags")
        .join(name);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::copy(&source, &path).expect("copy fixture");
    (dir, path)
}

/// `(album artist, composer)` as the next scan of `path` would store
/// them: the first through the scanner's own extractor, the second
/// through the custom-tag reader that fills `track_tag`.
fn scanned(path: &Path) -> (Option<String>, Option<String>) {
    use lofty::file::TaggedFileExt;
    let tagged = lofty::read_from_path(path).expect("read back");
    let album_artist = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .and_then(waveflow_core::scanner::extract_album_artist);
    let composer = waveflow_core::scanner::extra_tags::read_extra_tags(path)
        .expect("custom tags")
        .into_iter()
        .find(|(key, _)| key == COMPOSER_KEY)
        .map(|(_, value)| value);
    (album_artist, composer)
}

fn round_trip(name: &str) {
    let (_dir, path) = copy_fixture(name);
    let set = TrackEdit {
        title: Some("Bolero".into()),
        album_artist: Some("  Orchestre National; de Lyon ".into()),
        composer: Some(" Maurice Ravel ".into()),
        ..Default::default()
    };
    write_tags_to_file(&path, &set).expect("write");
    assert_eq!(
        scanned(&path),
        (
            // Whole, not split: it names the album's artist.
            Some("Orchestre National; de Lyon".to_string()),
            Some("Maurice Ravel".to_string())
        ),
        "{name}: what was written is what the scan reads"
    );

    // A save that names neither leaves both alone.
    let title_only = TrackEdit {
        title: Some("Boléro".into()),
        ..Default::default()
    };
    write_tags_to_file(&path, &title_only).expect("write");
    assert_eq!(
        scanned(&path).0.as_deref(),
        Some("Orchestre National; de Lyon"),
        "{name}: a title-only save kept the album artist"
    );

    let cleared = TrackEdit {
        album_artist: Some("   ".into()),
        composer: Some(String::new()),
        ..Default::default()
    };
    write_tags_to_file(&path, &cleared).expect("write");
    assert_eq!(scanned(&path), (None, None), "{name}: both cleared");
}

#[test]
fn an_id3v2_file_round_trips_both_fields() {
    round_trip("silence.mp3");
}

#[test]
fn a_flac_file_round_trips_both_fields() {
    round_trip("silence.flac");
}

#[test]
fn an_mp4_file_round_trips_both_fields() {
    round_trip("silence.m4a");
}

/// The comments the concrete-tag route exists to keep: an edit of the
/// two new fields must not take a custom one with it.
#[test]
fn a_flac_custom_comment_survives_the_edit() {
    let (_dir, path) = copy_fixture("silence.flac");
    {
        // Seeded through the comments' own writer, which leaves the
        // rest of the file alone.
        use lofty::config::{ParseOptions, WriteOptions};
        use lofty::file::AudioFile;
        use lofty::tag::TagExt;
        let mut file = std::fs::File::open(&path).unwrap();
        let flac = lofty::flac::FlacFile::read_from(&mut file, ParseOptions::new()).unwrap();
        drop(file);
        let mut comments = flac.vorbis_comments().cloned().unwrap_or_default();
        comments.push("CATALOGNUMBER".into(), "WF-001".into());
        comments
            .save_to_path(&path, WriteOptions::default())
            .unwrap();
    }
    let edit = TrackEdit {
        album_artist: Some("Someone".into()),
        composer: Some("Someone Else".into()),
        ..Default::default()
    };
    write_tags_to_file(&path, &edit).expect("write");
    let extra = waveflow_core::scanner::extra_tags::read_extra_tags(&path).unwrap();
    assert!(
        extra
            .iter()
            .any(|(k, v)| k == "CATALOGNUMBER" && v == "WF-001"),
        "custom comment lost: {extra:?}"
    );
}
