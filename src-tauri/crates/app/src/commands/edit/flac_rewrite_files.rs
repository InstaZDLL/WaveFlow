use super::*;
use lofty::picture::MimeType;
use std::path::{Path, PathBuf};
use symphonia::core::audio::sample::i24;
use symphonia::core::audio::{AudioBytes, GenericAudioBufferRef};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

const STREAMINFO: u8 = 0;
const PADDING: u8 = 1;
const APPLICATION: u8 = 2;
const SEEKTABLE: u8 = 3;
const COMMENTS: u8 = 4;
const PICTURE: u8 = 6;

const FRONT: u32 = 3;
const BACK: u32 = 4;

/// A FLAC holds nothing longer than this in one block.
const MAX_BLOCK: usize = 0x00ff_ffff;

/// A block as it sits in the file, its header left to [`file_from`].
type Block = (u8, Vec<u8>);

/// What a file is made of: everything before the frames, and the frames.
fn split(bytes: &[u8]) -> (Vec<Block>, Vec<u8>) {
    assert_eq!(&bytes[..4], b"fLaC");
    let mut at = 4;
    let mut blocks = Vec::new();
    loop {
        let header = &bytes[at..at + 4];
        let len = (header[1] as usize) << 16 | (header[2] as usize) << 8 | header[3] as usize;
        blocks.push((header[0] & 0x7f, bytes[at + 4..at + 4 + len].to_vec()));
        at += 4 + len;
        if header[0] & 0x80 != 0 {
            return (blocks, bytes[at..].to_vec());
        }
    }
}

fn file_from(blocks: &[Block], frames: &[u8]) -> Vec<u8> {
    let mut out = b"fLaC".to_vec();
    for (i, (ty, content)) in blocks.iter().enumerate() {
        let len = content.len() as u32;
        let last = if i + 1 == blocks.len() { 0x80 } else { 0 };
        out.extend_from_slice(&[ty | last, (len >> 16) as u8, (len >> 8) as u8, len as u8]);
        out.extend_from_slice(content);
    }
    out.extend_from_slice(frames);
    out
}

fn comments_block(comments: &[(&str, &str)]) -> Vec<u8> {
    let vendor = b"another tagger";
    let mut out = (vendor.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(vendor);
    out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for (key, value) in comments {
        let entry = format!("{key}={value}");
        out.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        out.extend_from_slice(entry.as_bytes());
    }
    out
}

/// A picture block, its fields spelled out so a test sees every byte go by.
fn picture_block(kind: u32, description: &str, data: &[u8]) -> Vec<u8> {
    let mime = b"image/png";
    let mut out = kind.to_be_bytes().to_vec();
    out.extend_from_slice(&(mime.len() as u32).to_be_bytes());
    out.extend_from_slice(mime);
    out.extend_from_slice(&(description.len() as u32).to_be_bytes());
    out.extend_from_slice(description.as_bytes());
    for field in [640u32, 480, 24, 0] {
        out.extend_from_slice(&field.to_be_bytes());
    }
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// The comments no generic tag can hold, beside two it can, one of them
/// holding two values, and a key in the case another tagger left it in.
const SEEDED_COMMENTS: &[(&str, &str)] = &[
    ("TITLE", "Before"),
    ("ARTIST", "Someone"),
    ("ARTIST", "Someone Else"),
    ("CATALOGNUMBER", "WF-001"),
    ("MY_OWN_FIELD", "kept"),
    ("replaygain_track_gain", "-6.50 dB"),
    (
        "MUSICBRAINZ_TRACKID",
        "11111111-2222-3333-4444-555555555555",
    ),
];

fn tone() -> (Block, Vec<u8>) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tags/tone.flac");
    let (blocks, frames) = split(&std::fs::read(source).expect("fixture"));
    let streaminfo = blocks
        .into_iter()
        .find(|(ty, _)| *ty == STREAMINFO)
        .expect("streaminfo");
    (streaminfo, frames)
}

/// A real FLAC with every kind of block a tagger leaves, and not one byte of
/// padding: whatever is written next has to move the frames.
pub(crate) fn flac_without_room() -> Vec<u8> {
    let (streaminfo, frames) = tone();
    file_from(
        &[
            streaminfo,
            (SEEKTABLE, vec![0xAB; 36]),
            (APPLICATION, b"wfTSsome private data".to_vec()),
            (COMMENTS, comments_block(SEEDED_COMMENTS)),
            (PICTURE, picture_block(FRONT, "front", &[0x11; 700])),
            (PICTURE, picture_block(BACK, "back", &[0x22; 900])),
        ],
        &frames,
    )
}

fn on_disk(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.flac");
    std::fs::write(&path, bytes).expect("seed");
    (dir, path)
}

fn blocks_of(blocks: &[Block], ty: u8) -> Vec<Vec<u8>> {
    blocks
        .iter()
        .filter(|(block_ty, _)| *block_ty == ty)
        .map(|(_, content)| content.clone())
        .collect()
}

/// The hash of what the file decodes to, through the registry playback uses.
fn decoded_hash(path: &Path) -> blake3::Hash {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let file = std::fs::File::open(path).expect("open");
    let mut format = waveflow_core::audio_format::probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("probe");
    let params = format
        .default_track(TrackType::Audio)
        .and_then(|track| track.codec_params.as_ref())
        .and_then(|params| params.audio())
        .expect("audio params")
        .clone();
    let mut decoder = waveflow_core::audio_format::opus::codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .expect("decoder");

    let mut hash = blake3::Hasher::new();
    let mut decoded = 0usize;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // How some readers end a stream. One an edit cut short still
            // shows, in a hash of fewer samples.
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(err) => panic!("next packet: {err:?}"),
        };
        let buffer = decoder.decode(&packet).expect("decode");
        macro_rules! bytes {
            ($b:expr, $s:ty) => {{
                let mut raw = vec![0u8; $b.byte_len_as::<$s>()];
                $b.copy_bytes_interleaved_as::<$s, _>(&mut raw[..]);
                decoded += raw.len();
                hash.update(&raw);
            }};
        }
        match &buffer {
            GenericAudioBufferRef::U8(b) => bytes!(b, u8),
            GenericAudioBufferRef::S8(b) => bytes!(b, u8),
            GenericAudioBufferRef::S16(b) => bytes!(b, i16),
            GenericAudioBufferRef::S24(b) => bytes!(b, i24),
            GenericAudioBufferRef::S32(b) => bytes!(b, i32),
            GenericAudioBufferRef::F32(b) => bytes!(b, f32),
            _ => panic!("sample format these streams do not produce"),
        }
    }
    assert!(
        decoded > 0,
        "a stream that decodes to nothing proves nothing"
    );
    hash.finalize()
}

/// The comments as the file holds them, without what `patch_file` makes of
/// them on the way.
fn comments_in(bytes: &[u8]) -> Vec<(String, String)> {
    let (blocks, _) = split(bytes);
    let block = &blocks_of(&blocks, COMMENTS)[0];
    let vendor_len = u32::from_le_bytes(block[..4].try_into().unwrap()) as usize;
    let mut at = 4 + vendor_len;
    let count = u32::from_le_bytes(block[at..at + 4].try_into().unwrap());
    at += 4;
    (0..count)
        .map(|_| {
            let len = u32::from_le_bytes(block[at..at + 4].try_into().unwrap()) as usize;
            at += 4;
            let entry = std::str::from_utf8(&block[at..at + len]).unwrap();
            at += len;
            let (key, value) = entry.split_once('=').unwrap();
            (key.to_string(), value.to_string())
        })
        .collect()
}

fn holds(comments: &[(String, String)], key: &str, value: &str) -> bool {
    comments
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case(key) && v == value)
}

fn no_scratch_file_left(dir: &tempfile::TempDir) {
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["track.flac"], "nothing beside the track");
}

/// Everything an edit of the comments leaves alone, against the file it
/// started from.
fn only_the_comments_changed(before: &[u8], path: &Path) {
    let after = std::fs::read(path).unwrap();
    let (old, old_frames) = split(before);
    let (new, new_frames) = split(&after);
    assert_eq!(old_frames, new_frames, "every frame, byte for byte");
    for ty in [STREAMINFO, SEEKTABLE, APPLICATION, PICTURE] {
        assert_eq!(
            blocks_of(&old, ty),
            blocks_of(&new, ty),
            "blocks of type {ty}"
        );
    }
    let comments = comments_in(&after);
    for (key, value) in SEEDED_COMMENTS.iter().filter(|(key, _)| *key != "TITLE") {
        assert!(holds(&comments, key, value), "{key} survives");
    }
}

fn title_too_long_for_the_room() -> TrackEdit {
    TrackEdit {
        title: Some("A title longer than the one in the file".into()),
        ..Default::default()
    }
}

#[test]
fn a_title_that_outgrows_the_room_is_written_and_touches_nothing_else() {
    let before = flac_without_room();
    let (dir, path) = on_disk(&before);
    let audio = decoded_hash(&path);

    patch_file(&path, &TagPatch::Fields(&title_too_long_for_the_room())).expect("edit");

    only_the_comments_changed(&before, &path);
    assert_eq!(decoded_hash(&path), audio, "the decoded stream");
    let after = std::fs::read(&path).unwrap();
    assert!(holds(
        &comments_in(&after),
        "TITLE",
        "A title longer than the one in the file"
    ));
    no_scratch_file_left(&dir);
}

#[test]
fn a_rewrite_leaves_room_for_the_next_edit() {
    // The next one fits, so the file keeps its length and is not copied.
    let (_dir, path) = on_disk(&flac_without_room());
    patch_file(&path, &TagPatch::Fields(&title_too_long_for_the_room())).expect("edit");
    let grown = std::fs::metadata(&path).unwrap().len();
    let (blocks, _) = split(&std::fs::read(&path).unwrap());
    assert_eq!(
        blocks.last().map(|(ty, _)| *ty),
        Some(PADDING),
        "padding last"
    );

    let edit = TrackEdit {
        title: Some("A title longer again than the one before this one was".into()),
        ..Default::default()
    };
    patch_file(&path, &TagPatch::Fields(&edit)).expect("edit");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), grown);
}

#[test]
fn long_lyrics_that_outgrow_the_room_are_written_whole() {
    let before = flac_without_room();
    let (dir, path) = on_disk(&before);
    let audio = decoded_hash(&path);
    let lyrics = "line of filler text\n".repeat(50_000);

    patch_file(
        &path,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised(&lyrics)),
    )
    .expect("edit");

    only_the_comments_changed(&before, &path);
    assert_eq!(decoded_hash(&path), audio, "the decoded stream");
    assert!(
        holds(
            &comments_in(&std::fs::read(&path).unwrap()),
            "UNSYNCEDLYRICS",
            &lyrics
        ),
        "the lyrics, to their last byte"
    );
    no_scratch_file_left(&dir);
}

#[test]
fn a_large_cover_replaces_the_front_and_keeps_every_other_image() {
    let before = flac_without_room();
    let (dir, path) = on_disk(&before);
    let audio = decoded_hash(&path);
    let cover = vec![0x33; 8 * 1024 * 1024];

    patch_file(
        &path,
        &TagPatch::Cover {
            bytes: &cover,
            mime: &MimeType::Png,
        },
    )
    .expect("edit");

    let after = std::fs::read(&path).unwrap();
    let (old, old_frames) = split(&before);
    let (new, new_frames) = split(&after);
    assert_eq!(old_frames, new_frames, "every frame, byte for byte");
    assert_eq!(decoded_hash(&path), audio, "the decoded stream");
    for ty in [STREAMINFO, SEEKTABLE, APPLICATION] {
        assert_eq!(
            blocks_of(&old, ty),
            blocks_of(&new, ty),
            "blocks of type {ty}"
        );
    }
    let pictures = blocks_of(&new, PICTURE);
    assert_eq!(pictures.len(), 2, "the new front and the back");
    let back = picture_block(BACK, "back", &[0x22; 900]);
    assert!(pictures.contains(&back), "the back cover, byte for byte");
    assert!(
        pictures
            .iter()
            .any(|block| block.ends_with(&cover) && block.starts_with(&FRONT.to_be_bytes())),
        "the new front cover"
    );
    let comments = comments_in(&after);
    for (key, value) in SEEDED_COMMENTS {
        assert!(holds(&comments, key, value), "{key} survives");
    }
    no_scratch_file_left(&dir);
}

#[test]
fn a_cover_on_a_padded_file_goes_through_too() {
    // Room or not: a cover has always been a rewrite.
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tags/silence.flac");
    let (dir, path) = on_disk(&std::fs::read(source).unwrap());
    patch_file(
        &path,
        &TagPatch::Cover {
            bytes: &[0x33; 64],
            mime: &MimeType::Png,
        },
    )
    .expect("edit");
    let (blocks, _) = split(&std::fs::read(&path).unwrap());
    assert_eq!(blocks_of(&blocks, PICTURE).len(), 1);
    no_scratch_file_left(&dir);
}

#[test]
fn a_cover_too_large_for_a_block_says_so_and_changes_nothing() {
    let before = flac_without_room();
    let (dir, path) = on_disk(&before);
    let cover = vec![0x33; MAX_BLOCK + 1];

    let err = patch_file(
        &path,
        &TagPatch::Cover {
            bytes: &cover,
            mime: &MimeType::Png,
        },
    )
    .expect_err("no block can hold it");

    assert!(
        err.to_string().contains("abnormally large"),
        "the reason reaches the user: {err}"
    );
    assert!(std::fs::read(&path).unwrap() == before, "the original");
    no_scratch_file_left(&dir);
}

#[test]
fn comments_too_large_for_a_block_say_so_and_change_nothing() {
    let before = flac_without_room();
    let (dir, path) = on_disk(&before);
    let lyrics = "x".repeat(MAX_BLOCK + 1);

    let err = patch_file(
        &path,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised(&lyrics)),
    )
    .expect_err("no block can hold them");

    assert!(
        err.to_string().contains("abnormally large"),
        "the reason reaches the user: {err}"
    );
    assert!(std::fs::read(&path).unwrap() == before, "the original");
    no_scratch_file_left(&dir);
}

#[test]
fn a_picture_lofty_cannot_read_survives_an_edit_of_the_comments() {
    // lofty discards one on its way through a save. The edit does not go
    // that way, and the block is still there, whole.
    let (streaminfo, frames) = tone();
    let broken = vec![0, 0, 0, 3, 0xff, 0xff, 0xff, 0xff, 1, 2, 3];
    let before = file_from(
        &[
            streaminfo,
            (COMMENTS, comments_block(SEEDED_COMMENTS)),
            (PICTURE, broken.clone()),
        ],
        &frames,
    );
    let (_dir, path) = on_disk(&before);

    patch_file(&path, &TagPatch::Fields(&title_too_long_for_the_room())).expect("edit");

    let (blocks, new_frames) = split(&std::fs::read(&path).unwrap());
    assert_eq!(blocks_of(&blocks, PICTURE), [broken]);
    assert_eq!(new_frames, frames);
}

/// An ID3v2 tag ahead of the stream, as some rippers leave one.
fn behind_id3v2(flac: &[u8]) -> Vec<u8> {
    use lofty::id3::v2::Id3v2Tag;
    use lofty::tag::{Accessor, TagExt};
    let mut tag = Id3v2Tag::default();
    tag.set_artist("From the ID3v2 tag".to_string());
    let mut out = Vec::new();
    tag.dump_to(&mut out, lofty::config::WriteOptions::default())
        .expect("dump");
    out.extend_from_slice(flac);
    out
}

#[test]
fn an_id3v2_tag_ahead_of_the_stream_stays_and_does_not_block_the_edit() {
    let flac = flac_without_room();
    let before = behind_id3v2(&flac);
    let (dir, path) = on_disk(&before);
    let prefix = before.len() - flac.len();
    let audio = decoded_hash(&path);

    patch_file(&path, &TagPatch::Fields(&title_too_long_for_the_room())).expect("edit");

    let after = std::fs::read(&path).unwrap();
    assert_eq!(after[..prefix], before[..prefix], "the tag stayed");
    let (old, old_frames) = split(&flac);
    let (new, new_frames) = split(&after[prefix..]);
    assert_eq!(old_frames, new_frames, "every frame, byte for byte");
    assert_eq!(
        blocks_of(&old, PICTURE).len(),
        blocks_of(&new, PICTURE).len()
    );
    let comments = comments_in(&after[prefix..]);
    assert!(holds(&comments, "MY_OWN_FIELD", "kept"));
    assert_eq!(decoded_hash(&path), audio, "the decoded stream");
    no_scratch_file_left(&dir);
}

#[test]
fn an_id3v2_tag_seeds_the_comments_a_file_does_not_have() {
    // The comments are what the library reads first, so comments holding
    // the edit alone would hide what the tag says.
    let (streaminfo, frames) = tone();
    let flac = file_from(&[streaminfo], &frames);
    let before = behind_id3v2(&flac);
    let (_dir, path) = on_disk(&before);
    let prefix = before.len() - flac.len();

    patch_file(&path, &TagPatch::Fields(&title_too_long_for_the_room())).expect("edit");

    let comments = comments_in(&std::fs::read(&path).unwrap()[prefix..]);
    assert!(holds(&comments, "ARTIST", "From the ID3v2 tag"));
    assert!(holds(
        &comments,
        "TITLE",
        "A title longer than the one in the file"
    ));
}

/// The other containers `patch_file` rewrites: the same two edits, with far
/// more to write than any of these has room for, and the stream unchanged.
#[test]
fn every_other_container_takes_long_lyrics_and_a_large_cover() {
    let lyrics = "line of filler text\n".repeat(20_000);
    let cover = vec![0x33; 4 * 1024 * 1024];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for fixture in [
        "app/tests/fixtures/tags/silence.mp3",
        "app/tests/fixtures/tags/silence.m4a",
        "app/tests/fixtures/tags/silence.ogg",
        "app/tests/fixtures/tags/silence.opus",
        "core/tests/fixtures/formats/aac.m4a",
        "core/tests/fixtures/formats/alac.m4a",
        "core/tests/fixtures/formats/opus.opus",
        "core/tests/fixtures/formats/vorbis.ogg",
        "core/tests/fixtures/lossless/s16_stereo_c2000.ape",
        "core/tests/fixtures/lossless/seek_blocks.wv",
    ] {
        let source = root.join(fixture);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(source.file_name().unwrap());
        std::fs::copy(&source, &path).expect("copy");
        let audio = decoded_hash(&path);

        patch_file(
            &path,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised(&lyrics)),
        )
        .unwrap_or_else(|err| panic!("{fixture}: lyrics: {err}"));
        assert_eq!(decoded_hash(&path), audio, "{fixture}: after the lyrics");

        patch_file(
            &path,
            &TagPatch::Cover {
                bytes: &cover,
                mime: &MimeType::Png,
            },
        )
        .unwrap_or_else(|err| panic!("{fixture}: cover: {err}"));
        assert_eq!(decoded_hash(&path), audio, "{fixture}: after the cover");

        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "{fixture}"
        );
    }
}
