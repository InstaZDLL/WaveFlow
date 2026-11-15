use super::*;
use crate::scanner::extract::{read_tagged_file, read_tagged_from};
use encoding_rs::{BIG5, EUC_KR, GBK, SHIFT_JIS};
use lofty::file::TaggedFileExt;
use lofty::tag::{Accessor, ItemKey};
use std::io::Cursor;

/// Four MPEG-1 Layer III frames: enough for lofty to take it for an MP3.
fn mp3_frames() -> Vec<u8> {
    let mut audio = Vec::new();
    for _ in 0..4 {
        let mut frame = vec![0u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        audio.extend(frame);
    }
    audio
}

fn syncsafe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 0x7f) as u8,
        ((n >> 14) & 0x7f) as u8,
        ((n >> 7) & 0x7f) as u8,
        (n & 0x7f) as u8,
    ]
}

/// An ID3v2 tag of the given version whose frames carry the given bytes under
/// the given encoding byte, followed by audio.
fn id3v2(version: u8, frames: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, encoding, text) in frames {
        let mut data = vec![*encoding];
        data.extend_from_slice(text);
        body.extend_from_slice(id.as_bytes());
        if version == 4 {
            body.extend(syncsafe(data.len()));
        } else {
            body.extend((data.len() as u32).to_be_bytes());
        }
        body.extend([0, 0]);
        body.extend(data);
    }
    let mut out = b"ID3".to_vec();
    out.extend([version, 0, 0]);
    out.extend(syncsafe(body.len()));
    out.extend(body);
    out.extend(mp3_frames());
    out
}

/// Audio followed by an ID3v1 trailer: fixed fields, no encoding to declare.
fn id3v1(title: &[u8], artist: &[u8], album: &[u8]) -> Vec<u8> {
    let mut trailer = vec![0u8; 128];
    trailer[..3].copy_from_slice(b"TAG");
    trailer[3..3 + title.len()].copy_from_slice(title);
    trailer[33..33 + artist.len()].copy_from_slice(artist);
    trailer[63..63 + album.len()].copy_from_slice(album);
    trailer[127] = 255;
    let mut out = mp3_frames();
    out.extend(trailer);
    out
}

fn encode(encoding: &'static Encoding, text: &str) -> Vec<u8> {
    let (bytes, _, unmappable) = encoding.encode(text);
    assert!(!unmappable, "{text}");
    bytes.into_owned()
}

/// `(title, artist, album)` of the file's tag, read through the shared reader.
fn read(bytes: Vec<u8>) -> (String, String, String) {
    let tagged = read_tagged_from(&mut Cursor::new(bytes), None).expect("readable");
    let tag = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .expect("a tag");
    (
        tag.title().unwrap_or_default().into_owned(),
        tag.artist().unwrap_or_default().into_owned(),
        tag.album().unwrap_or_default().into_owned(),
    )
}

const TITLE: &str = "夜曲";

fn assert_tag(bytes: Vec<u8>, title: &str, artist: &str, album: &str) {
    let (t, a, al) = read(bytes);
    assert_eq!(
        (t.as_str(), a.as_str(), al.as_str()),
        (title, artist, album)
    );
}

#[test]
fn an_id3v1_tag_in_gbk_is_read_as_chinese() {
    let bytes = id3v1(
        &encode(GBK, TITLE),
        &encode(GBK, "周杰伦"),
        &encode(GBK, "十一月的萧邦"),
    );
    // What lofty alone makes of it is the report's `Ò¹Çú`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.mp3");
    std::fs::write(&path, &bytes).unwrap();
    let unrepaired = lofty::read_from_path(&path).unwrap();
    assert_eq!(
        unrepaired.first_tag().unwrap().title().as_deref(),
        Some("Ò¹Çú")
    );
    assert_tag(bytes, TITLE, "周杰伦", "十一月的萧邦");
}

#[test]
fn an_id3v23_latin1_frame_filled_with_gbk_is_read_as_chinese() {
    for version in [3, 4] {
        let bytes = id3v2(
            version,
            &[
                ("TIT2", 0, &encode(GBK, TITLE)),
                ("TPE1", 0, &encode(GBK, "周杰伦")),
                ("TALB", 0, &encode(GBK, "十一月的萧邦")),
            ],
        );
        assert_tag(bytes, TITLE, "周杰伦", "十一月的萧邦");
    }
}

#[test]
fn big5_is_told_from_gbk_by_the_whole_tag() {
    let bytes = id3v2(
        3,
        &[
            ("TIT2", 0, &encode(BIG5, "夜曲")),
            ("TPE1", 0, &encode(BIG5, "周杰倫")),
            ("TALB", 0, &encode(BIG5, "十一月的蕭邦")),
        ],
    );
    assert_tag(bytes, "夜曲", "周杰倫", "十一月的蕭邦");
}

#[test]
fn shift_jis_is_read_as_japanese() {
    let bytes = id3v2(
        3,
        &[
            ("TIT2", 0, &encode(SHIFT_JIS, "夜に駆ける")),
            ("TPE1", 0, &encode(SHIFT_JIS, "宇多田ヒカル")),
            ("TALB", 0, &encode(SHIFT_JIS, "初恋")),
        ],
    );
    assert_tag(bytes, "夜に駆ける", "宇多田ヒカル", "初恋");
}

#[test]
fn euc_kr_is_read_as_korean() {
    let bytes = id3v1(
        &encode(EUC_KR, "사랑해요"),
        &encode(EUC_KR, "아이유"),
        &encode(EUC_KR, "라일락"),
    );
    assert_tag(bytes, "사랑해요", "아이유", "라일락");
}

/// A title alone is too short to guess from: the artist and the album of the
/// same file settle it, which no per-field guess could.
#[test]
fn the_guess_is_made_over_every_field_of_the_file() {
    let title = encode(BIG5, "夜曲");
    // The title alone reads cleanly in more than one encoding.
    assert!(GBK
        .decode_without_bom_handling_and_without_replacement(&title)
        .is_some());
    let bytes = id3v2(
        3,
        &[
            ("TIT2", 0, &title),
            ("TPE1", 0, &encode(BIG5, "周杰倫")),
            ("TALB", 0, &encode(BIG5, "葉惠美")),
        ],
    );
    assert_tag(bytes, "夜曲", "周杰倫", "葉惠美");
}

#[test]
fn legitimate_latin1_is_left_alone() {
    let witnesses: [(&[u8], &[u8], &[u8]); 5] = [
        (b"Caf\xe9", b"Beyonc\xe9", b""),
        (b"\xc6r\xf8sk\xf8bing", b"Sigur R\xf3s", b""),
        (b"Se\xf1or", b"Se\xf1or Coconut", b"El Baile Alem\xe1n"),
        (b"\xc9t\xe9", b"", b""),
        (b"\xd1", b"\xc5", b"\xd6"),
    ];
    for (title, artist, album) in witnesses {
        for bytes in [
            id3v1(title, artist, album),
            id3v2(
                3,
                &[("TIT2", 0, title), ("TPE1", 0, artist), ("TALB", 0, album)],
            ),
        ] {
            let latin1 = |b: &[u8]| b.iter().map(|&c| char::from(c)).collect::<String>();
            assert_tag(bytes, &latin1(title), &latin1(artist), &latin1(album));
        }
    }
}

/// UTF-8 and UTF-16 frames never come near the repair.
#[test]
fn text_that_declares_its_encoding_is_untouched() {
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "Ærøskøbing 夜曲".encode_utf16() {
        utf16.extend(unit.to_le_bytes());
    }
    let bytes = id3v2(3, &[("TIT2", 1, &utf16), ("TPE1", 0, b"Artist")]);
    assert_tag(bytes, "Ærøskøbing 夜曲", "Artist", "");
    let bytes = id3v2(
        4,
        &[
            ("TIT2", 3, "夜曲".as_bytes()),
            ("TPE1", 3, "周杰伦".as_bytes()),
        ],
    );
    assert_tag(bytes, "夜曲", "周杰伦", "");
}

/// The multi-artist split sees the repaired credit: the separator is `"; "`
/// in whatever encoding, and a credit repaired after the split would have
/// been cut in the middle of a character.
#[test]
fn the_credit_is_repaired_before_it_is_split() {
    let credit = encode(GBK, "周杰伦; 方文山");
    let bytes = id3v2(
        3,
        &[
            ("TIT2", 0, &encode(GBK, TITLE)),
            ("TPE1", 0, &credit),
            ("TALB", 0, &encode(GBK, "十一月的萧邦")),
        ],
    );
    let tagged = read_tagged_from(&mut Cursor::new(bytes), None).unwrap();
    let tag = tagged.primary_tag().unwrap();
    let (credit, _) = crate::scanner::extract::extract_artist_credit(tag);
    assert_eq!(credit.as_deref(), Some("周杰伦; 方文山"));
}

#[test]
fn the_other_fields_are_repaired_and_their_order_kept() {
    let bytes = id3v2(
        3,
        &[
            ("TIT2", 0, &encode(GBK, TITLE)),
            ("TPE2", 0, &encode(GBK, "周杰伦")),
            ("TCON", 0, &encode(GBK, "流行")),
            ("TCOM", 0, &encode(GBK, "周杰伦")),
        ],
    );
    let tagged = read_tagged_from(&mut Cursor::new(bytes), None).unwrap();
    let tag = tagged.primary_tag().unwrap();
    assert_eq!(tag.get_string(ItemKey::AlbumArtist), Some("周杰伦"));
    assert_eq!(tag.get_string(ItemKey::Genre), Some("流行"));
    assert_eq!(tag.get_string(ItemKey::Composer), Some("周杰伦"));
}

#[test]
fn a_file_on_disk_is_repaired_like_one_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.mp3");
    std::fs::write(
        &path,
        id3v1(
            &encode(GBK, TITLE),
            &encode(GBK, "周杰伦"),
            &encode(GBK, "叶惠美"),
        ),
    )
    .unwrap();
    let tagged = read_tagged_file(&path).unwrap();
    assert_eq!(tagged.first_tag().unwrap().title().as_deref(), Some(TITLE));
}

#[test]
fn no_text_and_ascii_text_ask_for_nothing() {
    assert_eq!(recover(&[]), None);
    assert_eq!(recover(&[b"\xe9".to_vec()]), None);
}

/// Declared UTF-8, filled in GBK: lofty refuses such a file whole, and the
/// scanner listed it by its file name with no tag. The same tag as a
/// Latin-1 one comes out right now, on a reader and on a file.
#[test]
fn a_frame_declaring_utf8_over_gbk_is_recovered() {
    for version in [3, 4] {
        let bytes = id3v2(
            version,
            &[
                ("TIT2", 3, &encode(GBK, TITLE)),
                ("TPE1", 3, &encode(GBK, "周杰伦")),
                ("TALB", 3, &encode(GBK, "十一月的萧邦")),
            ],
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.mp3");
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            lofty::read_from_path(&path).is_err(),
            "lofty alone refuses it"
        );
        let tagged = read_tagged_file(&path).expect("recovered from disk");
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some(TITLE));
        assert_eq!(tag.album().as_deref(), Some("十一月的萧邦"));
        assert_tag(bytes, TITLE, "周杰伦", "十一月的萧邦");
    }
}

/// The same declaration over Latin-1 bytes is a Western tag, read as one.
#[test]
fn a_frame_declaring_utf8_over_latin1_is_read_as_latin1() {
    let bytes = id3v2(4, &[("TIT2", 3, b"Caf\xe9"), ("TPE1", 3, b"Beyonc\xe9")]);
    assert_tag(bytes, "Café", "Beyoncé", "");
}

/// Only the frame that is wrong is read differently: its neighbours keep
/// the encoding they declared.
#[test]
fn a_valid_utf8_frame_next_to_a_broken_one_is_kept() {
    let bytes = id3v2(
        4,
        &[
            ("TIT2", 3, &encode(GBK, TITLE)),
            ("TPE1", 3, "周杰伦".as_bytes()),
        ],
    );
    let (title, artist, _) = read(bytes);
    assert_eq!(artist, "周杰伦");
    assert_eq!(title, TITLE);
}

/// A tag the walk cannot follow (unsynchronised) is refused as before.
#[test]
fn an_unsynchronised_tag_is_not_guessed_at() {
    let mut bytes = id3v2(4, &[("TIT2", 3, &encode(GBK, TITLE))]);
    bytes[5] |= 0x80;
    assert!(read_tagged_from(&mut Cursor::new(bytes), None).is_err());
}

/// Compatibility jamo are hangul too: a title of "ㅋㅋ" must not rule out
/// EUC-KR for the tag it is in.
#[test]
fn compatibility_jamo_are_text_a_korean_title_is_made_of() {
    assert!(cjk_text("ㅋㅋ 사랑해요"));
    assert!(decode_clean(EUC_KR, &encode(EUC_KR, "ㅋㅋ")).is_some_and(|text| cjk_text(&text)));
}

/// A repaired comment keeps its language and its description: they are what
/// tells one comment from another.
#[test]
fn a_repaired_item_keeps_its_language_and_description() {
    use lofty::tag::{ItemKey, ItemValue, TagItem, TagType};
    let mut tag = Tag::new(TagType::Id3v2);
    let text = |bytes: &[u8]| -> String { bytes.iter().map(|&b| char::from(b)).collect() };
    tag.insert_text(ItemKey::TrackTitle, text(&encode(GBK, TITLE)));
    tag.insert_text(ItemKey::TrackArtist, text(&encode(GBK, "周杰伦")));
    let mut comment = TagItem::new(
        ItemKey::Comment,
        ItemValue::Text(text(&encode(GBK, "好听"))),
    );
    comment.set_description("note".to_string());
    tag.push_unchecked(comment);

    repair_tag(&mut tag);

    let item = tag.get(ItemKey::Comment).expect("the comment");
    assert_eq!(item.value(), &ItemValue::Text("好听".to_string()));
    assert_eq!(item.description(), "note");
}

/// v2.4 frame flags: a grouped frame has a byte before the encoding, which
/// the walk does not follow, so it is left to be refused as before.
#[test]
fn a_grouped_utf8_frame_is_not_patched() {
    let mut bytes = id3v2(4, &[("TIT2", 3, &encode(GBK, TITLE))]);
    // Frame flags are the two bytes after the size: set the grouping bit.
    bytes[10 + 9] |= 0x40;
    assert!(read_tagged_from(&mut Cursor::new(bytes), None).is_err());
}

#[test]
fn the_patched_reader_zeroes_its_offsets_whatever_the_reads() {
    use std::io::{Read, Seek, SeekFrom};
    let bytes = vec![0xffu8; 100];
    // Out of order on purpose: the reader sorts them.
    let mut patched = utf8_frames::Patched::new(Cursor::new(bytes), vec![99, 5, 50, 0, 6]).unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0u8; 7];
    loop {
        let n = patched.read(&mut chunk).unwrap();
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&chunk[..n]);
    }
    let zeroes: Vec<usize> = (0..seen.len()).filter(|i| seen[*i] == 0).collect();
    assert_eq!(zeroes, vec![0, 5, 6, 50, 99]);

    patched.seek(SeekFrom::Start(48)).unwrap();
    let mut three = [0u8; 3];
    patched.read_exact(&mut three).unwrap();
    assert_eq!(three, [0xff, 0xff, 0]);
}
