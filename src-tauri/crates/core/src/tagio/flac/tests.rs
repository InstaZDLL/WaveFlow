use super::*;
use lofty::ogg::tag::VorbisComments;

fn block(ty: u8, content: &[u8], last: bool) -> Vec<u8> {
    let len = content.len() as u32;
    let mut out = vec![
        if last { ty | 0x80 } else { ty },
        (len >> 16) as u8,
        (len >> 8) as u8,
        len as u8,
    ];
    out.extend_from_slice(content);
    out
}

/// A file shaped like a tagged FLAC: the magic, a stand-in
/// STREAMINFO, a comment block, a padding block, then bytes standing
/// in for audio frames.
fn flac_file(padding: usize, audio: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.flac");
    let mut comments = VorbisComments::default();
    comments.push("TITLE".to_string(), "Before".to_string());

    let mut bytes = b"fLaC".to_vec();
    bytes.extend_from_slice(&block(0, &[0u8; 34], false));
    bytes.extend_from_slice(&block(
        FLAC_BLOCK_VORBIS_COMMENT,
        &encode_vorbis_comments(&comments),
        false,
    ));
    bytes.extend_from_slice(&block(FLAC_BLOCK_PADDING, &vec![0u8; padding], true));
    bytes.extend_from_slice(audio);
    std::fs::write(&path, &bytes).expect("seed");
    (dir, path)
}

fn open(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open")
}

#[test]
fn an_edit_that_fits_the_padding_leaves_the_audio_alone() {
    // The claim the benchmark in the issue rests on: the write
    // touches the metadata region and nothing else, so the cost is
    // the region rather than the file.
    let audio: Vec<u8> = (0..8192u32).map(|i| (i % 241) as u8 + 1).collect();
    let (_dir, path) = flac_file(1024, &audio);
    let before_len = std::fs::metadata(&path).expect("meta").len();

    let mut comments = VorbisComments::default();
    comments.push("TITLE".to_string(), "After the edit".to_string());
    comments.push("ARTIST".to_string(), "Someone".to_string());

    let mut file = open(&path);
    assert!(try_flac_in_place(&mut file, &comments).expect("in place"));
    file.sync_all().expect("sync");
    drop(file);

    let after = std::fs::read(&path).expect("read");
    assert_eq!(
        after.len() as u64,
        before_len,
        "the file did not change length"
    );
    assert_eq!(
        &after[after.len() - audio.len()..],
        &audio[..],
        "every audio byte is where it was"
    );

    // And the region still parses as blocks, ending on a last-block
    // flag — a region that did not close would take the first audio
    // bytes with it on the next read.
    let mut file = open(&path);
    let blocks = read_flac_blocks(&mut file).expect("read").expect("blocks");
    let region: usize = blocks.iter().map(|b| b.content.len() + 4).sum();
    assert_eq!(region + 4 + audio.len(), after.len());
    let comment = blocks
        .iter()
        .find(|b| b.ty == FLAC_BLOCK_VORBIS_COMMENT)
        .expect("a comment block");
    assert!(
        comment.content.windows(14).any(|w| w == b"After the edit"),
        "the new title is in the block"
    );
}

#[test]
fn an_edit_too_big_for_the_padding_asks_for_the_rewrite() {
    // No slack at all, and far more to write than there was: this is
    // the case that has to move the file, and saying so is the only
    // safe answer.
    let (_dir, path) = flac_file(0, b"audio bytes");
    let before = std::fs::read(&path).expect("read");

    let mut comments = VorbisComments::default();
    comments.push("TITLE".to_string(), "T".repeat(4096));

    let mut file = open(&path);
    assert!(!try_flac_in_place(&mut file, &comments).expect("in place"));
    drop(file);
    assert_eq!(
        std::fs::read(&path).expect("read"),
        before,
        "refusing must not have written anything"
    );
}

#[test]
fn a_file_that_is_not_flac_is_left_alone() {
    // An ID3v2-prefixed FLAC lands here too: the magic is not at
    // byte zero, so the region offsets would all be wrong.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("not.flac");
    std::fs::write(&path, b"ID3\x04\x00\x00\x00\x00\x00\x00rest").expect("seed");
    let mut file = open(&path);
    assert!(!try_flac_in_place(&mut file, &VorbisComments::default()).expect("in place"));
}

#[test]
fn a_comment_set_carrying_a_picture_asks_for_the_rewrite() {
    // FLAC normally keeps pictures in their own blocks, which is why
    // this path copies those through untouched. But a VorbisComments
    // can carry them too, and the encoder here writes only the
    // vendor and the items — so encoding a set that has pictures
    // would drop them. Losing a cover to a title edit is the one
    // thing this path must never do.
    use lofty::ogg::OggPictureStorage;
    use lofty::picture::{Picture, PictureInformation, PictureType};

    let audio: Vec<u8> = vec![3; 512];
    let (_dir, path) = flac_file(4096, &audio);
    let before = std::fs::read(&path).expect("read");

    let mut comments = VorbisComments::default();
    comments.push("TITLE".to_string(), "After".to_string());
    let picture = Picture::unchecked(vec![0xFF; 64])
        .pic_type(PictureType::CoverFront)
        .mime_type(lofty::picture::MimeType::Jpeg)
        .build();
    comments
        .insert_picture(picture, Some(PictureInformation::default()))
        .expect("insert");

    let mut file = open(&path);
    assert!(!try_flac_in_place(&mut file, &comments).expect("in place"));
    drop(file);
    assert_eq!(
        std::fs::read(&path).expect("read"),
        before,
        "declining must not have written anything"
    );
}

#[test]
fn a_truncated_file_asks_for_the_rewrite_instead_of_failing() {
    // A block header that runs off the end is this function saying
    // "not a container I can splice", not an I/O failure the edit
    // should die on.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cut.flac");
    let mut bytes = b"fLaC".to_vec();
    bytes.extend_from_slice(&block(0, &[0u8; 34], false));
    // A header promising far more content than the file holds.
    bytes.extend_from_slice(&[FLAC_BLOCK_VORBIS_COMMENT, 0x00, 0xFF, 0x00]);
    bytes.extend_from_slice(b"only a few bytes");
    std::fs::write(&path, &bytes).expect("seed");

    let mut file = open(&path);
    assert!(read_flac_blocks(&mut file).expect("read").is_none());
    let mut file = open(&path);
    assert!(
        !try_flac_in_place(&mut file, &VorbisComments::default()).expect("in place"),
        "a truncated file declines rather than erroring"
    );
}

#[test]
fn the_blocks_we_did_not_edit_are_copied_through_byte_for_byte() {
    // The rule the whole tag path follows: correcting a title never
    // costs a cover or a MusicBrainz identifier.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rich.flac");
    let picture: Vec<u8> = (0..300u32).map(|i| (i % 255) as u8).collect();
    let seektable: Vec<u8> = vec![0xAB; 180];

    let mut comments = VorbisComments::default();
    comments.push("TITLE".to_string(), "Before".to_string());
    let mut bytes = b"fLaC".to_vec();
    bytes.extend_from_slice(&block(0, &[0u8; 34], false));
    bytes.extend_from_slice(&block(3, &seektable, false));
    bytes.extend_from_slice(&block(6, &picture, false));
    bytes.extend_from_slice(&block(
        FLAC_BLOCK_VORBIS_COMMENT,
        &encode_vorbis_comments(&comments),
        false,
    ));
    bytes.extend_from_slice(&block(FLAC_BLOCK_PADDING, &vec![0u8; 512], true));
    std::fs::write(&path, &bytes).expect("seed");

    let mut edited = VorbisComments::default();
    edited.push("TITLE".to_string(), "After".to_string());
    let mut file = open(&path);
    assert!(try_flac_in_place(&mut file, &edited).expect("in place"));
    drop(file);

    let mut file = open(&path);
    let blocks = read_flac_blocks(&mut file).expect("read").expect("blocks");
    assert_eq!(
        blocks.iter().find(|b| b.ty == 6).expect("picture").content,
        picture,
        "the cover survived a title edit"
    );
    assert_eq!(
        blocks
            .iter()
            .find(|b| b.ty == 3)
            .expect("seektable")
            .content,
        seektable,
        "the seek table survived a title edit"
    );
}

/// A rewrite's two files, for [`check_rewrite`] to compare.
fn check(before: &[u8], after: &[u8]) -> io::Result<()> {
    let dir = tempfile::tempdir().expect("tempdir");
    let before_path = dir.path().join("before.flac");
    let after_path = dir.path().join("after.flac");
    std::fs::write(&before_path, before).expect("seed");
    std::fs::write(&after_path, after).expect("seed");
    check_rewrite(&mut open(&before_path), &mut open(&after_path))
}

fn picture(kind: u32, data: &[u8]) -> Vec<u8> {
    let mime = b"image/png";
    let mut out = kind.to_be_bytes().to_vec();
    out.extend_from_slice(&(mime.len() as u32).to_be_bytes());
    out.extend_from_slice(mime);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out
}

fn flac_of(blocks: &[(u8, &[u8])], audio: &[u8]) -> Vec<u8> {
    let mut bytes = b"fLaC".to_vec();
    for (i, (ty, content)) in blocks.iter().enumerate() {
        bytes.extend_from_slice(&block(*ty, content, i + 1 == blocks.len()));
    }
    bytes.extend_from_slice(audio);
    bytes
}

const FRONT: u32 = 3;
const BACK: u32 = 4;

#[test]
fn a_rewrite_that_only_touched_the_tags_passes() {
    let front = picture(FRONT, b"front");
    let back = picture(BACK, b"back");
    let before = flac_of(
        &[
            (0, &[1; 34]),
            (3, &[2; 18]),
            (4, b"old"),
            (6, &front),
            (6, &back),
        ],
        b"frames",
    );
    // lofty moves the blocks around, and a cover edit replaces the front.
    let new_front = picture(FRONT, b"another front");
    let after = flac_of(
        &[
            (0, &[1; 34]),
            (3, &[2; 18]),
            (1, &[0; 32]),
            (4, b"new"),
            (6, &back),
            (6, &new_front),
        ],
        b"frames",
    );
    check(&before, &after).expect("nothing the edit may not touch moved");
}

#[test]
fn a_rewrite_is_refused_for_each_thing_it_may_not_touch() {
    let front = picture(FRONT, b"front");
    let back = picture(BACK, b"back");
    let streaminfo = [1u8; 34];
    let seektable = [2u8; 18];
    let whole: &[(u8, &[u8])] = &[
        (0, &streaminfo),
        (3, &seektable),
        (4, b"old"),
        (6, &front),
        (6, &back),
    ];
    let before = flac_of(whole, b"frames");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("one frame byte changed", flac_of(whole, b"frameZ")),
        ("the frames cut short", flac_of(whole, b"frame")),
        (
            "the STREAMINFO changed",
            flac_of(
                &[
                    (0, &[9; 34]),
                    (3, &seektable),
                    (4, b"old"),
                    (6, &front),
                    (6, &back),
                ],
                b"frames",
            ),
        ),
        (
            "the seek table gone",
            flac_of(
                &[(0, &streaminfo), (4, b"old"), (6, &front), (6, &back)],
                b"frames",
            ),
        ),
        (
            "the back cover gone",
            flac_of(
                &[(0, &streaminfo), (3, &seektable), (4, b"old"), (6, &front)],
                b"frames",
            ),
        ),
        (
            "the front cover gone",
            flac_of(
                &[(0, &streaminfo), (3, &seektable), (4, b"old"), (6, &back)],
                b"frames",
            ),
        ),
        ("no longer FLAC", b"RIFFnot this at all".to_vec()),
    ];
    for (what, after) in cases {
        let err = check(&before, &after).expect_err(what);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{what}");
    }
}

#[test]
fn a_picture_that_does_not_parse_has_to_stay_whole() {
    let broken = [0u8, 0, 0, 3, 0xff, 0xff];
    let before = flac_of(&[(0, &[1; 34]), (6, &broken)], b"frames");
    check(&before, &before).expect("kept");
    let after = flac_of(&[(0, &[1; 34])], b"frames");
    assert!(check(&before, &after).is_err(), "lost");
}

#[test]
fn the_check_sees_the_stream_behind_an_id3v2_tag() {
    let mut tag = b"ID3\x04\x00\x00\x00\x00\x00\x04".to_vec();
    tag.extend_from_slice(&[0u8; 4]);
    let mut before = tag.clone();
    before.extend_from_slice(&flac_of(&[(0, &[1; 34])], b"frames"));
    let mut same = tag.clone();
    same.extend_from_slice(&flac_of(&[(0, &[1; 34]), (1, &[0; 8])], b"frames"));
    check(&before, &same).expect("the same stream");
    let mut changed = tag;
    changed.extend_from_slice(&flac_of(&[(0, &[1; 34])], b"framez"));
    assert!(check(&before, &changed).is_err(), "the frames differ");
}

#[test]
fn a_file_that_is_not_flac_has_nothing_to_answer_for() {
    check(b"RIFF and whatever follows", b"something else entirely").expect("not ours to judge");
}

#[test]
fn a_rewrite_that_damages_the_frames_leaves_the_original_in_place() {
    use std::io::{Seek, SeekFrom, Write};

    let before = flac_of(&[(0, &[1; 34]), (4, b"old")], b"frames");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.flac");
    std::fs::write(&path, &before).expect("seed");

    let outcome: Result<(), io::Error> = crate::tagio::rewrite_via_temp(&path, |file| {
        file.seek(SeekFrom::End(-1))?;
        file.write_all(b"Z")
    });

    assert_eq!(
        outcome.expect_err("refused").kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(std::fs::read(&path).expect("read"), before);
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("list").count(),
        1,
        "nothing beside the track"
    );
}

#[test]
fn growing_the_region_moves_the_frames_and_leaves_room() {
    let audio: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let (_dir, path) = flac_file(16, &audio);
    let mut comments = VorbisComments::default();
    comments.push("TITLE".to_string(), "T".repeat(4096));

    let mut file = open(&path);
    assert!(grow_comments_on_copy(&mut file, &comments).expect("grow"));

    let blocks = read_flac_blocks(&mut file).expect("read").expect("blocks");
    assert_eq!(blocks.last().map(|b| b.ty), Some(FLAC_BLOCK_PADDING));
    assert_eq!(
        blocks.last().map(|b| b.content.len() as u64),
        Some(FLAC_REWRITE_PADDING)
    );
    let after = std::fs::read(&path).expect("read");
    let region = 4 + region_len(&blocks) as usize;
    assert_eq!(
        &after[region..],
        &audio[..],
        "every frame, after the region"
    );
}

#[test]
fn growing_declines_what_it_cannot_write_or_would_shrink() {
    use lofty::ogg::OggPictureStorage;
    use lofty::picture::{Picture, PictureInformation, PictureType};

    // Not at byte zero: an ID3v2 tag sits there.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("not.flac");
    std::fs::write(&path, b"ID3\x04\x00\x00\x00\x00\x00\x00rest").expect("seed");
    assert!(!grow_comments_on_copy(&mut open(&path), &VorbisComments::default()).expect("grow"));

    // Pictures in the comments: the encoder here would drop them.
    let (_dir, path) = flac_file(0, b"audio");
    let before = std::fs::read(&path).expect("read");
    let mut comments = VorbisComments::default();
    comments
        .insert_picture(
            Picture::unchecked(vec![0xFF; 64])
                .pic_type(PictureType::CoverFront)
                .mime_type(lofty::picture::MimeType::Jpeg)
                .build(),
            Some(PictureInformation::default()),
        )
        .expect("insert");
    assert!(!grow_comments_on_copy(&mut open(&path), &comments).expect("grow"));
    assert_eq!(std::fs::read(&path).expect("read"), before);

    // More padding than the rewrite would leave.
    let (_dir, path) = flac_file(FLAC_REWRITE_PADDING as usize * 2, b"audio");
    let before = std::fs::read(&path).expect("read");
    let mut comments = VorbisComments::default();
    comments.push("TITLE".to_string(), "T".to_string());
    assert!(!grow_comments_on_copy(&mut open(&path), &comments).expect("grow"));
    assert_eq!(std::fs::read(&path).expect("read"), before);
}
