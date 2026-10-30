//! The workshop's writes on real files of the three families the
//! fixtures cover. Every test works on a copy in a temporary directory.

use super::read::{to_db_edit, to_edit};
use super::*;
use crate::commands::edit::MUSICBRAINZ_UFID_OWNER;
use std::path::{Path, PathBuf};

const RELEASE: &str = "9dce399d-3ec7-34a4-97d0-b4bac54e5f08";
const GROUP: &str = "1b022e01-4da6-387b-8658-8678046e4cef";
const RECORDING: &str = "5fb524f1-8cc8-4c04-a921-e34c0a911ea7";
const RELEASE_TRACK: &str = "e18075e4-fb74-3896-8967-d4b1e683702f";
const ARTISTS: &str = "5b11f4ce-a62d-471e-81fc-a69a8278c7da; 6b11f4ce-a62d-471e-81fc-a69a8278c7da";
const ALBUM_ARTIST: &str = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";

fn copy_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tags")
        .join(name);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::copy(&source, &path).expect("copy fixture");
    (dir, path)
}

fn workshop_values() -> Values {
    [
        (Field::Title, "Smells Like Teen Spirit"),
        (Field::Artist, "Nirvana; Guest"),
        (Field::Album, "Nevermind"),
        (Field::AlbumArtist, "Nirvana"),
        (Field::Year, "1991"),
        (Field::TrackNumber, "1"),
        (Field::TrackTotal, "13"),
        (Field::DiscNumber, "1"),
        (Field::DiscTotal, "2"),
        (Field::Genre, "Grunge; Alternative Rock"),
        (Field::Composer, "Kurt Cobain"),
        (Field::MbReleaseId, RELEASE),
        (Field::MbReleaseGroupId, GROUP),
        (Field::MbRecordingId, RECORDING),
        (Field::MbReleaseTrackId, RELEASE_TRACK),
        (Field::MbArtistId, ARTISTS),
        (Field::MbAlbumArtistId, ALBUM_ARTIST),
    ]
    .into_iter()
    .map(|(f, v)| (f, v.to_owned()))
    .collect()
}

fn write(path: &Path, values: &Values) {
    let edit = to_edit(values);
    patch_file(path, &TagPatch::Fields(&edit)).expect("write");
}

/// The bytes that are the audio, not the tags: what a tag write must
/// never touch.
fn audio_payload(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    match path.extension().and_then(|e| e.to_str()) {
        Some("flac") => {
            // "fLaC", then metadata blocks until the one flagged last.
            assert_eq!(&bytes[..4], b"fLaC");
            let mut at = 4;
            loop {
                let header = bytes[at];
                let len = u32::from_be_bytes([0, bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                at += 4 + len as usize;
                if header & 0x80 != 0 {
                    break;
                }
            }
            bytes[at..].to_vec()
        }
        Some("mp3") => {
            let mut at = 0;
            if bytes.starts_with(b"ID3") {
                let size = bytes[6..10]
                    .iter()
                    .fold(0usize, |acc, b| (acc << 7) | (*b as usize & 0x7f));
                let footer = if bytes[5] & 0x10 != 0 { 10 } else { 0 };
                at = 10 + size + footer;
            }
            // Padding a tag leaves before the first frame is the tag's.
            while bytes.get(at) == Some(&0) {
                at += 1;
            }
            bytes[at..].to_vec()
        }
        Some("m4a") => {
            // The `mdat` box at the top level holds the samples.
            let mut at = 0;
            while at + 8 <= bytes.len() {
                let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
                if &bytes[at + 4..at + 8] == b"mdat" {
                    return bytes[at + 8..at + size].to_vec();
                }
                assert!(size >= 8, "a malformed box");
                at += size;
            }
            panic!("no mdat box");
        }
        other => panic!("no payload reader for {other:?}"),
    }
}

fn every_field_round_trips(name: &str) {
    let (_dir, path) = copy_fixture(name);
    let audio = audio_payload(&path);
    let wanted = workshop_values();
    write(&path, &wanted);

    let read = read_values(&path).expect("read back");
    assert_eq!(read, wanted, "{name}: what was written is what is read");
    assert_eq!(
        audio_payload(&path),
        audio,
        "{name}: the audio bytes are untouched"
    );
    // The engine's own reader finds the release the workshop wrote, so
    // its next pass needs no search.
    assert_eq!(
        waveflow_core::enrichment::tags::release_mbid(&path).as_deref(),
        Some(RELEASE),
        "{name}: the enrichment engine reads the release back"
    );
}

#[test]
fn every_field_round_trips_through_an_mp3() {
    every_field_round_trips("silence.mp3");
}

#[test]
fn every_field_round_trips_through_a_flac() {
    every_field_round_trips("silence.flac");
}

#[test]
fn every_field_round_trips_through_an_m4a() {
    every_field_round_trips("silence.m4a");
}

#[test]
fn the_identifiers_go_where_each_format_keeps_them() {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;

    let (_mp3_dir, mp3) = copy_fixture("silence.mp3");
    write(&mp3, &workshop_values());
    let mut file = std::fs::File::open(&mp3).unwrap();
    let parsed = lofty::mpeg::MpegFile::read_from(&mut file, ParseOptions::new()).unwrap();
    let tag = parsed.id3v2().expect("an ID3v2 tag");
    // The recording in the `UFID` frame MusicBrainz owns.
    let ufid = tag
        .into_iter()
        .find_map(|frame| match frame {
            lofty::id3::v2::Frame::UniqueFileIdentifier(f) => Some(f),
            _ => None,
        })
        .expect("a UFID frame");
    assert_eq!(ufid.owner, MUSICBRAINZ_UFID_OWNER);
    assert_eq!(ufid.identifier.as_ref(), RECORDING.as_bytes());
    // The others as `TXXX` frames, a list in one frame.
    let txxx: Vec<(String, String)> = tag
        .into_iter()
        .filter_map(|frame| match frame {
            lofty::id3::v2::Frame::UserText(f) => {
                Some((f.description.to_string(), f.content.to_string()))
            }
            _ => None,
        })
        .collect();
    let get = |d: &str| {
        txxx.iter()
            .find(|(desc, _)| desc == d)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(get("MusicBrainz Album Id").as_deref(), Some(RELEASE));
    assert_eq!(get("MusicBrainz Release Group Id").as_deref(), Some(GROUP));
    assert_eq!(
        get("MusicBrainz Release Track Id").as_deref(),
        Some(RELEASE_TRACK)
    );
    assert_eq!(
        get("MusicBrainz Artist Id").as_deref(),
        Some(ARTISTS.replace("; ", "\0").as_str())
    );
    assert_eq!(
        get("MusicBrainz Album Artist Id").as_deref(),
        Some(ALBUM_ARTIST)
    );

    let (_flac_dir, flac) = copy_fixture("silence.flac");
    write(&flac, &workshop_values());
    let mut file = std::fs::File::open(&flac).unwrap();
    let parsed = lofty::flac::FlacFile::read_from(&mut file, ParseOptions::new()).unwrap();
    let comments = parsed.vorbis_comments().expect("Vorbis comments");
    let get = |key: &str| comments.get_all(key).map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(get("MUSICBRAINZ_ALBUMID"), [RELEASE]);
    assert_eq!(get("MUSICBRAINZ_RELEASEGROUPID"), [GROUP]);
    // "Track Id" names the recording: the tagger's historical choice.
    assert_eq!(get("MUSICBRAINZ_TRACKID"), [RECORDING]);
    assert_eq!(get("MUSICBRAINZ_RELEASETRACKID"), [RELEASE_TRACK]);
    // A list as one comment per identifier.
    assert_eq!(
        get("MUSICBRAINZ_ARTISTID"),
        [
            "5b11f4ce-a62d-471e-81fc-a69a8278c7da",
            "6b11f4ce-a62d-471e-81fc-a69a8278c7da"
        ]
    );
    assert_eq!(get("MUSICBRAINZ_ALBUMARTISTID"), [ALBUM_ARTIST]);

    let (_m4a_dir, m4a) = copy_fixture("silence.m4a");
    write(&m4a, &workshop_values());
    let mut file = std::fs::File::open(&m4a).unwrap();
    let parsed = lofty::mp4::Mp4File::read_from(&mut file, ParseOptions::new()).unwrap();
    let ilst = parsed.ilst().expect("an ilst");
    let freeform = |name: &str| {
        let ident = lofty::mp4::AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: name.to_owned().into(),
        };
        ilst.get(&ident)
            .map(|atom| {
                atom.data()
                    .filter_map(|d| match d {
                        lofty::mp4::AtomData::UTF8(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    assert_eq!(freeform("MusicBrainz Album Id"), [RELEASE]);
    assert_eq!(freeform("MusicBrainz Track Id"), [RECORDING]);
    assert_eq!(freeform("MusicBrainz Release Track Id"), [RELEASE_TRACK]);
}

#[test]
fn writing_the_old_values_back_restores_the_file() {
    for name in ["silence.mp3", "silence.flac", "silence.m4a"] {
        let (_dir, path) = copy_fixture(name);
        let before = read_values(&path).expect("read");
        let wanted = workshop_values();
        let diffs = diff(&before, &wanted);
        assert!(!diffs.is_empty());
        write(&path, &wanted);

        // What an undo writes: each journaled field's old value, an
        // absent one as an empty value, which removes it.
        let restore: Values = diffs.iter().map(|d| (d.field, d.old.clone())).collect();
        write(&path, &restore);
        assert_eq!(
            read_values(&path).expect("read"),
            before,
            "{name}: the undo puts back exactly what was there"
        );
    }
}

#[test]
fn a_field_left_out_is_not_touched() {
    let (_dir, path) = copy_fixture("silence.flac");
    write(&path, &workshop_values());
    let only_title: Values = [(Field::Title, "In Bloom".to_owned())]
        .into_iter()
        .collect();
    write(&path, &only_title);
    let read = read_values(&path).unwrap();
    assert_eq!(
        read.get(&Field::Title).map(String::as_str),
        Some("In Bloom")
    );
    assert_eq!(
        read.get(&Field::MbRecordingId).map(String::as_str),
        Some(RECORDING)
    );
    assert_eq!(
        read.get(&Field::Composer).map(String::as_str),
        Some("Kurt Cobain")
    );
}

#[test]
fn a_cleared_title_is_the_file_name_in_the_library() {
    let values: Values = [(Field::Title, String::new())].into_iter().collect();
    // The file loses its title…
    assert_eq!(to_edit(&values).title.as_deref(), Some(""));
    // …and the row gets the name the scanner would give it.
    let db = to_db_edit(&values, Path::new("/music/03 Lithium.flac"));
    assert_eq!(db.title.as_deref(), Some("03 Lithium"));
}

#[test]
fn a_dff_is_shown_and_never_written() {
    let err = read_values(Path::new("/music/track.dff")).unwrap_err();
    assert!(err.contains("DFF"));
}
