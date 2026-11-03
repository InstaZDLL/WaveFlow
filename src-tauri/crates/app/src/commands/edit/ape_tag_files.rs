use super::*;
use lofty::ape::{ApeFile, ApeItem, ApeTag};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::MimeType;
use lofty::prelude::*;
use lofty::tag::{ItemValue, Tag};
use lofty::wavpack::WavPackFile;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use waveflow_core::scanner::extra_tags::{read_extra_tags, COMPOSER_KEY};

/// A file in each container that carries APEv2, with a tag on it as another
/// tool would have left it: fields of ours, and one no reader here models.
const FIELDS: &[(&str, &str)] = &[
    ("Title", "Before"),
    ("MY_OWN_FIELD", "keep me"),
    ("REPLAYGAIN_TRACK_GAIN", "-6.50 dB"),
];

fn seeded_tag() -> ApeTag {
    let mut tag = ApeTag::new();
    for (key, value) in FIELDS {
        tag.insert(ApeItem::new((*key).into(), ItemValue::Text((*value).into())).expect("item"));
    }
    tag
}

fn copy_tagged(name: &str) -> (tempfile::TempDir, PathBuf, Vec<u8>) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/lossless")
        .join(name);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::copy(&source, &path).expect("copy fixture");
    let audio = std::fs::read(&path).expect("read");
    save_tag(&path, seeded_tag());
    (dir, path, audio)
}

/// Save `tag` as another tool would: straight into the file, past `patch_file`.
fn save_tag(path: &Path, tag: ApeTag) {
    let mut handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open");
    if path.extension().is_some_and(|ext| ext == "wv") {
        let mut file = WavPackFile::read_from(&mut handle, ParseOptions::new()).expect("read");
        file.set_ape(tag);
        handle.seek(SeekFrom::Start(0)).expect("rewind");
        file.save_to(&mut handle, WriteOptions::default())
            .expect("save");
    } else {
        let mut file = ApeFile::read_from(&mut handle, ParseOptions::new()).expect("read");
        file.set_ape(tag);
        handle.seek(SeekFrom::Start(0)).expect("rewind");
        file.save_to(&mut handle, WriteOptions::default())
            .expect("save");
    }
}

fn read_tag(path: &Path) -> Tag {
    waveflow_core::scanner::extract::read_tagged_file(path)
        .expect("read back")
        .primary_tag()
        .expect("a tag")
        .clone()
}

fn each_container(check: impl Fn(&str, &Path, &[u8])) {
    for name in ["s16_stereo.wv", "s16_stereo_c2000.ape"] {
        let (_dir, path, audio) = copy_tagged(name);
        check(name, &path, &audio);
    }
}

/// Both formats keep their tag after the audio, so what a save must not touch
/// is simply every byte the file started with.
fn assert_audio_untouched(name: &str, path: &Path, audio: &[u8]) {
    let after = std::fs::read(path).expect("read");
    assert!(after.starts_with(audio), "{name}: the audio changed");
}

#[test]
fn an_edit_writes_the_fields_and_keeps_what_it_does_not_model() {
    each_container(|name, path, audio| {
        let edit = TrackEdit {
            title: Some("After".into()),
            album_artist: Some("The Album Artist".into()),
            composer: Some("A Composer".into()),
            track_number: Some(4),
            musicbrainz: MusicBrainzEdit {
                recording_id: Some("5fb524f1-8cc8-4c04-a921-e34c0a911ea7".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        write_tags_to_file(path, &edit).expect("write");

        let tag = read_tag(path);
        assert_eq!(tag.title().as_deref(), Some("After"), "{name}");
        assert_eq!(tag.track(), Some(4), "{name}");
        assert_eq!(
            waveflow_core::scanner::extract_album_artist(&tag).as_deref(),
            Some("The Album Artist"),
            "{name}"
        );
        assert_eq!(
            tag.get_string(ItemKey::MusicBrainzRecordingId),
            Some("5fb524f1-8cc8-4c04-a921-e34c0a911ea7"),
            "{name}: identifiers go in the file like any other field"
        );
        let extra = read_extra_tags(path).expect("custom tags");
        assert!(
            extra
                .iter()
                .any(|(k, v)| k == COMPOSER_KEY && v == "A Composer"),
            "{name}: {extra:?}"
        );
        assert!(
            extra
                .iter()
                .any(|(k, v)| k == "MY_OWN_FIELD" && v == "keep me"),
            "{name}: a field no reader models must survive a save, got {extra:?}"
        );
        assert_eq!(
            waveflow_core::scanner::replay_gain::extract_replay_gain(&tag).track_gain_db,
            Some(-6.5),
            "{name}: another tool's ReplayGain must survive a save"
        );
        assert_audio_untouched(name, path, audio);
    });
}

/// lofty refuses to save a file with an ID3v2 tag ahead of the stream.
#[test]
fn an_id3v2_tag_ahead_of_the_stream_does_not_stop_an_edit() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/lossless/s16_stereo_c2000.ape");
    let audio = std::fs::read(&source).expect("read");

    use id3::TagLike;

    let mut id3 = id3::Tag::new();
    id3.set_title("From ID3");
    id3.set_artist("Kept Artist");
    let mut bytes = Vec::new();
    id3.write_to(&mut bytes, id3::Version::Id3v24).expect("id3");
    let tag_len = bytes.len();
    bytes.extend_from_slice(&audio);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.ape");
    std::fs::write(&path, &bytes).expect("write");

    let edit = TrackEdit {
        title: Some("After".into()),
        ..Default::default()
    };
    write_tags_to_file(&path, &edit).expect("write");

    let tag = read_tag(&path);
    assert_eq!(tag.title().as_deref(), Some("After"));
    assert_eq!(tag.artist().as_deref(), Some("Kept Artist"));
    let after = std::fs::read(&path).expect("read");
    assert_eq!(&after[tag_len..tag_len + audio.len()], &audio[..]);
}

/// The tag Monkey's Audio itself writes: items and a footer, with no header.
#[test]
fn an_edit_over_a_tag_with_no_header_costs_no_audio() {
    let items: &[u8] = b"\x05\0\0\0\0\0\0\0Title\0Hello";
    let mut footer = b"APETAGEX".to_vec();
    footer.extend_from_slice(&2000u32.to_le_bytes());
    footer.extend_from_slice(&((items.len() + 32) as u32).to_le_bytes());
    footer.extend_from_slice(&1u32.to_le_bytes());
    footer.extend_from_slice(&(1u32 << 30).to_le_bytes());
    footer.extend_from_slice(&[0; 8]);

    for name in ["s16_stereo.wv", "s16_stereo_c2000.ape"] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../core/tests/fixtures/lossless")
            .join(name);
        let audio = std::fs::read(&source).expect("read");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, [&audio[..], items, &footer].concat()).expect("write");

        let edit = TrackEdit {
            title: Some("After".into()),
            ..Default::default()
        };
        write_tags_to_file(&path, &edit).expect("write");
        assert_eq!(read_tag(&path).title().as_deref(), Some("After"), "{name}");
        assert_audio_untouched(name, &path, &audio);
    }
}

/// A saved title goes through the generic tag and back, and lofty's key table
/// has no entry for a rating here: the item has to stay behind in the tag it
/// came out of, half stars and another program's item alike.
#[test]
fn a_saved_title_keeps_the_rating() {
    each_container(|name, path, audio| {
        let edit = TrackEdit {
            title: Some("After".into()),
            ..Default::default()
        };
        for rating in [26u8, 128, 204, 255] {
            patch_file(path, &TagPatch::Rating(Some(rating))).expect("rating");
            write_tags_to_file(path, &edit).expect("write");
            assert_eq!(
                crate::commands::scan::read_file_rating(path).expect("rating read"),
                Some(rating),
                "{name}: rating {rating}"
            );
        }

        let mut tag = seeded_tag();
        tag.insert(ApeItem::new("Rating".into(), ItemValue::Text("80".into())).expect("item"));
        save_tag(path, tag);
        write_tags_to_file(path, &edit).expect("write");
        assert_eq!(
            crate::commands::scan::read_file_rating(path).expect("rating read"),
            Some(204),
            "{name}: a rating another program wrote"
        );
        assert_eq!(read_tag(path).title().as_deref(), Some("After"), "{name}");

        assert_audio_untouched(name, path, audio);
    });
}

#[test]
fn a_rating_a_cover_and_lyrics_reach_the_file_and_read_back() {
    // A real PNG header is what lofty's generic picture path wants to see.
    const COVER: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
    each_container(|name, path, audio| {
        patch_file(path, &TagPatch::Rating(Some(204))).expect("rating");
        assert_eq!(
            waveflow_core::scanner::extract_rating(&read_tag(path)),
            Some(204),
            "{name}: rating"
        );
        assert!(
            read_extra_tags(path)
                .expect("custom tags")
                .iter()
                .all(|(key, _)| key != "RATING"),
            "{name}: the stars are not a custom column"
        );
        // Every value the star widget produces survives the round trip.
        for rating in [26u8, 51, 77, 102, 128, 153, 179, 230, 255] {
            patch_file(path, &TagPatch::Rating(Some(rating))).expect("rating");
            assert_eq!(
                waveflow_core::scanner::extract_rating(&read_tag(path)),
                Some(rating),
                "{name}: rating {rating}"
            );
        }
        patch_file(path, &TagPatch::Rating(None)).expect("clearing");
        assert_eq!(
            waveflow_core::scanner::extract_rating(&read_tag(path)),
            None
        );

        patch_file(
            path,
            &TagPatch::Cover {
                bytes: COVER,
                mime: &MimeType::Png,
            },
        )
        .expect("cover");
        let tag = read_tag(path);
        assert_eq!(tag.picture_count(), 1, "{name}: cover");
        assert_eq!(tag.pictures()[0].data(), COVER, "{name}: cover bytes");

        patch_file(
            path,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised("the words")),
        )
        .expect("lyrics");
        let tag = read_tag(path);
        assert_eq!(
            tag.get_string(ItemKey::UnsyncLyrics),
            Some("the words"),
            "{name}: lyrics"
        );
        assert_eq!(tag.picture_count(), 1, "{name}: the lyrics kept the cover");

        // The slot TTML needs: an arbitrary string, under its own key.
        patch_file(
            path,
            &TagPatch::Lyrics(LyricsSlot::Arbitrary("<tt>the words</tt>")),
        )
        .expect("arbitrary lyrics");
        assert_eq!(
            read_tag(path).get_string(ItemKey::Lyrics),
            Some("<tt>the words</tt>"),
            "{name}: arbitrary lyrics"
        );
        assert_audio_untouched(name, path, audio);
    });
}
