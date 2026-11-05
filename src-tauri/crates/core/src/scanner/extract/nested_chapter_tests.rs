//! A tag whose chapter frames nest ends the process in every ID3v2 parser the app
//! holds, and a stack overflow cannot be caught. Through the readers the scanner
//! really calls, on stacks smaller than any the app runs them on.

use super::*;
use crate::scanner::extra_tags::{read_concrete_tags, read_concrete_tags_from};
use std::io::Cursor;
use std::path::PathBuf;

/// Smaller than the 2 MiB of a blocking task. What is refused never reaches a
/// parser, so it needs no stack to speak of.
const SMALL_STACK: usize = 256 * 1024;
/// What one level of honest nesting needs from lofty in a debug build, with margin.
const HONEST_STACK: usize = 1024 * 1024;

fn synchsafe(value: usize) -> [u8; 4] {
    [
        ((value >> 21) & 0x7f) as u8,
        ((value >> 14) & 0x7f) as u8,
        ((value >> 7) & 0x7f) as u8,
        (value & 0x7f) as u8,
    ]
}

fn frame(major: u8, id: &[u8; 4], flags: u16, body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    if major == 4 {
        out.extend_from_slice(&synchsafe(body.len()));
    } else {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    }
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn title(major: u8, text: &str) -> Vec<u8> {
    let mut body = vec![3_u8];
    body.extend_from_slice(text.as_bytes());
    frame(major, b"TIT2", 0, &body)
}

fn chapter_prefix(id: &str, start_ms: u32) -> Vec<u8> {
    let mut body = id.as_bytes().to_vec();
    body.push(0);
    body.extend_from_slice(&start_ms.to_be_bytes());
    body.extend_from_slice(&(start_ms + 999).to_be_bytes());
    body.extend_from_slice(&[0xff; 8]);
    body
}

fn toc_prefix() -> Vec<u8> {
    let mut body = b"toc\0".to_vec();
    body.extend_from_slice(&[0x03, 1]);
    body.extend_from_slice(b"c\0");
    body
}

/// `depth` chapter frames, each inside the last, cycling through `ids`.
fn nested(major: u8, ids: &[&[u8; 4]], depth: usize) -> Vec<u8> {
    let mut inner = title(major, "deep");
    for level in 0..depth {
        let id = ids[level % ids.len()];
        let mut body = if id == b"CHAP" {
            chapter_prefix("c", 0)
        } else {
            toc_prefix()
        };
        body.extend_from_slice(&inner);
        inner = frame(major, id, 0, &body);
    }
    inner
}

fn tag_with(major: u8, flags: u8, frames: &[u8]) -> Vec<u8> {
    let mut out = b"ID3".to_vec();
    out.extend_from_slice(&[major, 0, flags]);
    out.extend_from_slice(&synchsafe(frames.len()));
    out.extend_from_slice(frames);
    out
}

fn tag(frames: &[u8]) -> Vec<u8> {
    tag_with(4, 0, frames)
}

/// Enough MPEG-1 Layer III frames for lofty and the decoder to call it audio.
fn audio() -> Vec<u8> {
    let mut frame = vec![0xff, 0xfb, 0x90, 0x00];
    frame.resize(417, 0);
    frame.repeat(40)
}

fn mp3(tag: &[u8]) -> Vec<u8> {
    [tag, audio().as_slice()].concat()
}

/// What `lofty` inflates with no compression library needed to write it: zlib's
/// stored blocks, then the checksum. Three bytes to the block, so that no name of four
/// is left to read in the bytes, as it would not be in anything really compressed.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(3).peekable();
    while let Some(chunk) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn wav(chunk: &[u8]) -> Vec<u8> {
    let mut body = b"WAVEfmt ".to_vec();
    body.extend_from_slice(&16_u32.to_le_bytes());
    for field in [1_u16, 1] {
        body.extend_from_slice(&field.to_le_bytes());
    }
    body.extend_from_slice(&8000_u32.to_le_bytes());
    body.extend_from_slice(&16_000_u32.to_le_bytes());
    for field in [2_u16, 16] {
        body.extend_from_slice(&field.to_le_bytes());
    }
    body.extend_from_slice(b"data");
    body.extend_from_slice(&1600_u32.to_le_bytes());
    body.extend_from_slice(&[0; 1600]);
    body.extend_from_slice(b"id3 ");
    body.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    body.extend_from_slice(chunk);
    if chunk.len() % 2 == 1 {
        body.push(0);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn aiff(chunk: &[u8]) -> Vec<u8> {
    let mut body = b"AIFFCOMM".to_vec();
    body.extend_from_slice(&18_u32.to_be_bytes());
    body.extend_from_slice(&1_u16.to_be_bytes());
    body.extend_from_slice(&800_u32.to_be_bytes());
    body.extend_from_slice(&16_u16.to_be_bytes());
    body.extend_from_slice(&[0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&1608_u32.to_be_bytes());
    body.extend_from_slice(&[0; 1608]);
    body.extend_from_slice(b"ID3 ");
    body.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
    body.extend_from_slice(chunk);
    if chunk.len() % 2 == 1 {
        body.push(0);
    }
    let mut out = b"FORM".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn flac_behind(tag: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend_from_slice(b"fLaC");
    out.extend_from_slice(&[0x80, 0, 0, 34]);
    out.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&((8000_u64 << 44) | (15 << 36) | 4096).to_be_bytes());
    out.extend_from_slice(&[0; 16]);
    out
}

fn on_stack<T: Send + 'static>(stack: usize, read: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(read)
        .expect("spawn")
        .join()
        .expect("join")
}

fn write_in(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write fixture");
    path
}

/// Every reader of the scan's path, on a stack none of them would survive this on.
/// The test binary dying is the failure, there is nothing to catch.
fn assert_refused_everywhere(name: &str, bytes: Vec<u8>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), name, &bytes);
    let label = name.to_string();
    on_stack(SMALL_STACK, move || {
        assert!(
            read_tagged_file(&path).is_err(),
            "{label}: the tag reader refuses it"
        );
        assert!(
            read_concrete_tags(&path).is_none(),
            "{label}: the custom frames refuse it"
        );
        assert!(
            read_tagged_from(&mut Cursor::new(bytes.clone()), None).is_err(),
            "{label}: a file on a share is refused the same"
        );
        assert!(
            read_concrete_tags_from(Cursor::new(bytes), None).is_none(),
            "{label}: its custom frames as well"
        );
        assert!(
            probe_playable_stream(&path).is_none(),
            "{label}: and so does the last resort, which would list it"
        );
    });
}

#[test]
fn chapters_nested_like_a_small_file_can_are_refused() {
    // About two kilobytes. A 2 MiB stack dies at 64 levels of this in a release
    // build, and at 32 in a debug one.
    assert_refused_everywhere("small.mp3", mp3(&tag(&nested(4, &[b"CHAP"], 64))));
}

#[test]
fn chapters_nested_as_deep_as_a_megabyte_allows_are_refused() {
    assert_refused_everywhere("deep.mp3", mp3(&tag(&nested(4, &[b"CHAP"], 2_000))));
}

#[test]
fn a_table_of_contents_nests_the_same_way() {
    assert_refused_everywhere("ctoc.mp3", mp3(&tag(&nested(4, &[b"CTOC"], 64))));
    assert_refused_everywhere("mixed.mp3", mp3(&tag(&nested(4, &[b"CHAP", b"CTOC"], 64))));
}

#[test]
fn id3v2_3_nests_the_same_way() {
    assert_refused_everywhere("v23.mp3", mp3(&tag_with(3, 0, &nested(3, &[b"CHAP"], 64))));
}

#[test]
fn nesting_hidden_behind_junk_or_a_first_tag_is_found() {
    let hostile = tag(&nested(4, &[b"CHAP"], 64));
    assert_refused_everywhere(
        "junk.mp3",
        mp3(&[vec![0x5a; 512], hostile.clone()].concat()),
    );
    assert_refused_everywhere(
        "second.mp3",
        mp3(&[tag(&title(4, "first")), hostile.clone()].concat()),
    );
    assert_refused_everywhere("zeros.mp3", mp3(&[vec![0; 64 * 1024], hostile].concat()));
}

#[test]
fn nesting_inside_a_frame_a_parser_would_step_into_is_found() {
    // The first frame ends early for a parser that stops at the zero. A walk that
    // trusted its size would jump over what that parser reads next.
    let chain = nested(4, &[b"CHAP"], 64);
    let mut body = chapter_prefix("a", 0);
    body.extend_from_slice(&[0; 10]);
    body.extend_from_slice(&chain);
    assert_refused_everywhere("padded.mp3", mp3(&tag(&frame(4, b"CHAP", 0, &body))));

    // A frame the parser may not read to its end, then a text frame holding the rest.
    let mut lure = frame(4, b"RVA2", 0, &[0; 4]);
    let mut text = vec![3_u8];
    text.extend_from_slice(&chain);
    lure.extend_from_slice(&frame(4, b"TIT2", 0, &text));
    assert_refused_everywhere("lure.mp3", mp3(&tag(&lure)));
}

#[test]
fn nesting_inside_a_compressed_chapter_is_refused_unread() {
    let mut inside = chapter_prefix("c", 0);
    inside.extend_from_slice(&nested(4, &[b"CHAP"], 64));
    let mut body = synchsafe(inside.len()).to_vec();
    body.extend_from_slice(&zlib_stored(&inside));
    assert_refused_everywhere("zlib.mp3", mp3(&tag(&frame(4, b"CHAP", 0x0009, &body))));
}

#[test]
fn nesting_in_the_other_containers_is_refused() {
    let hostile = tag(&nested(4, &[b"CHAP"], 64));
    assert_refused_everywhere("aiff.aiff", aiff(&hostile));
    assert_refused_everywhere("flac.flac", flac_behind(&hostile));

    // symphonia has no ID3v2 in a WAV, so there the last resort still plays it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), "wav.wav", &wav(&hostile));
    on_stack(SMALL_STACK, move || {
        assert!(read_tagged_file(&path).is_err());
        assert!(read_concrete_tags(&path).is_none());
    });
}

/// The bound is not the file size: a chain of three reads, whatever surrounds it.
#[test]
fn a_chapter_or_two_inside_a_chapter_still_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), "two.mp3", &mp3(&tag(&nested(4, &[b"CHAP"], 3))));
    on_stack(HONEST_STACK, move || {
        assert!(read_tagged_file(&path).is_ok());
    });
}

fn episode(major: u8, chapters: usize, name: impl Fn(usize) -> String) -> Vec<u8> {
    let mut frames = title(major, "Episode");
    for index in 0..chapters {
        let mut body = chapter_prefix(&format!("c{index}"), index as u32 * 1000);
        body.extend_from_slice(&title(major, &name(index)));
        frames.extend_from_slice(&frame(major, b"CHAP", 0, &body));
    }
    frames
}

fn assert_reads(name: &str, bytes: Vec<u8>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), name, &bytes);
    let label = name.to_string();
    on_stack(HONEST_STACK, move || {
        assert!(read_tagged_file(&path).is_ok(), "{label}: tags");
        assert!(
            read_concrete_tags(&path).is_some(),
            "{label}: custom frames"
        );
        assert!(probe_playable_stream(&path).is_some(), "{label}: audio");
    });
}

#[test]
fn a_long_honest_episode_reads_however_its_chapters_are_called() {
    // Thousands of chapters side by side nest nothing, and a title that spells the
    // frame's name is a title: "CHAPTER 19" even reads as a compressed frame.
    assert_reads(
        "wide.mp3",
        mp3(&tag(&episode(4, 2_000, |i| format!("Chapter {i}")))),
    );
    assert_reads(
        "shouted.mp3",
        mp3(&tag(&episode(4, 300, |i| format!("CHAPTER {i}")))),
    );
    assert_reads(
        "shouted_v23.mp3",
        mp3(&tag_with(
            3,
            0,
            &episode(3, 300, |i| format!("CHAPTER {i}")),
        )),
    );
}

#[test]
fn an_honest_tag_the_whole_of_which_is_unsynchronised_reads() {
    // A 2.3 tag's unsynchronisation shifts every length after a 0xff. Sizes of 255
    // bytes put one in each header, and the walk has to see the frames as parsers do.
    let long = "x".repeat(250);
    let frames = episode(3, 40, |i| format!("CHAPTER {i} {long}"));
    let mut wire = Vec::with_capacity(frames.len());
    for byte in frames {
        wire.push(byte);
        if byte == 0xff {
            wire.push(0);
        }
    }
    assert_reads("unsync.mp3", mp3(&tag_with(3, 0x80, &wire)));
}

#[test]
fn a_cover_full_of_the_frame_name_is_a_cover() {
    let mut body = vec![0_u8];
    body.extend_from_slice(b"image/jpeg\0\x03\0");
    body.extend_from_slice(&b"CHAP\xff\xff\xff\xff\x00\x08".repeat(10_000));
    assert_reads("cover.mp3", mp3(&tag(&frame(4, b"APIC", 0, &body))));
}

/// What a radio or a feed serves: bytes that come once, from a host nobody chose
/// with this in mind.
struct LiveStream(Cursor<Vec<u8>>);

impl std::io::Read for LiveStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer)
    }
}

impl std::io::Seek for LiveStream {
    fn seek(&mut self, _: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

impl symphonia::core::io::MediaSource for LiveStream {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

fn open_live(bytes: Vec<u8>) -> symphonia::core::errors::Result<bool> {
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;

    let stream =
        MediaSourceStream::new(Box::new(LiveStream(Cursor::new(bytes))), Default::default());
    let mut format = crate::audio_format::probe().probe(
        &Hint::new(),
        stream,
        Default::default(),
        Default::default(),
    )?;
    let has_tags = format.metadata().current().is_some();
    Ok(has_tags)
}

#[test]
fn a_live_stream_is_refused_on_its_tag_and_a_good_one_keeps_its_tags() {
    on_stack(SMALL_STACK, || {
        assert!(open_live(mp3(&tag(&nested(4, &[b"CHAP"], 2_000)))).is_err());
        // One that cannot seek has no second look at chunks it has read past.
        assert!(open_live(aiff(&tag(&nested(4, &[b"CHAP"], 2_000)))).is_err());
    });
    on_stack(HONEST_STACK, || {
        assert_eq!(
            open_live(mp3(&tag(&title(4, "Now playing")))).ok(),
            Some(true)
        );
    });
}

#[test]
fn an_honest_aiff_keeps_its_tag_chunk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), "tagged.aiff", &aiff(&tag(&title(4, "Kept"))));
    on_stack(HONEST_STACK, move || {
        assert!(read_tagged_file(&path).is_ok());
        assert!(probe_playable_stream(&path).is_some());
    });
}

#[test]
fn a_comment_lofty_stops_reading_early_hides_nothing() {
    // UTF-16 with a byte-order mark: an empty description, then a text lofty ends at
    // its first terminator, reading what is left as the next frame.
    let mut body = vec![1];
    body.extend_from_slice(b"eng");
    body.extend_from_slice(&[0xff, 0xfe, 0, 0]);
    body.extend_from_slice(&[0xff, 0xfe, b'A', 0, 0, 0]);
    body.extend_from_slice(&nested(4, &[b"CHAP"], 64));
    assert_refused_everywhere("comment.mp3", mp3(&tag(&frame(4, b"COMM", 0, &body))));
}

#[test]
fn a_tag_behind_a_footer_flag_or_an_ape_tag_is_found() {
    let hostile = tag(&nested(4, &[b"CHAP"], 64));
    // The bit means nothing in 2.2, and lofty does not step over a footer between
    // two tags of its own.
    let bare_v22 = [b'I', b'D', b'3', 2, 0, 0x10, 0, 0, 0, 0].to_vec();
    assert_refused_everywhere("v22.mp3", mp3(&[bare_v22, hostile.clone()].concat()));
    let with_footer = tag_with(4, 0x10, &title(4, "b"));
    assert_refused_everywhere(
        "footer.mp3",
        mp3(&[tag(&title(4, "a")), with_footer, hostile.clone()].concat()),
    );

    // Far behind what the APE tag declares, since lofty walks its items instead.
    let mut ape = b"APETAGEX".to_vec();
    ape.extend_from_slice(&2000_u32.to_le_bytes());
    ape.extend_from_slice(&5000_u32.to_le_bytes());
    ape.extend_from_slice(&100_u32.to_le_bytes());
    ape.extend_from_slice(&0xa000_0000_u32.to_le_bytes());
    ape.extend_from_slice(&[0; 8]);
    for index in 0..100 {
        ape.extend_from_slice(&1000_u32.to_le_bytes());
        ape.extend_from_slice(&2_u32.to_le_bytes());
        ape.extend_from_slice(format!("item{index}\0").as_bytes());
        ape.extend_from_slice(&[0x5a; 1000]);
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = mp3(&[ape, hostile].concat());
    let path = write_in(dir.path(), "ape.mp3", &bytes);
    on_stack(SMALL_STACK, move || {
        assert!(read_tagged_file(&path).is_err());
        assert!(read_concrete_tags(&path).is_none());
    });
}

#[test]
fn a_chapter_whose_flags_hide_behind_unsynchronisation_is_refused() {
    let mut inside = chapter_prefix("c", 0);
    inside.extend_from_slice(&nested(4, &[b"CHAP"], 64));
    let mut packed = synchsafe(inside.len()).to_vec();
    packed.extend_from_slice(&zlib_stored(&inside));
    // Unsynchronised, a parser reads this length as 0xff 0x01 0x02 0x03, then flags.
    let mut child = b"CHAP".to_vec();
    child.extend_from_slice(&[0xff, 0, 0x01, 0x02, 0x03, 0, 0x09]);
    child.extend_from_slice(&packed);
    let mut body = chapter_prefix("a", 0);
    body.extend_from_slice(&child);
    assert_refused_everywhere("unsync.mp3", mp3(&tag(&frame(4, b"CHAP", 0x0002, &body))));
}

#[test]
fn tables_of_contents_that_name_each_other_are_refused() {
    // No nesting in the bytes: symphonia recurses on the entries.
    let chain = |tables: usize| {
        let mut frames = Vec::new();
        for index in 0..tables {
            let mut body = format!("t{index}\0").into_bytes();
            body.extend_from_slice(&[if index == 0 { 0x03 } else { 0x01 }, 1]);
            body.extend_from_slice(format!("t{}\0", index + 1).as_bytes());
            frames.extend_from_slice(&frame(4, b"CTOC", 0, &body));
        }
        frames
    };
    // The one that recurses goes first: lofty has no such path and would answer first.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), "tables.mp3", &mp3(&tag(&chain(100_000))));
    on_stack(SMALL_STACK, move || {
        assert!(probe_playable_stream(&path).is_none());
    });
    assert_refused_everywhere("tables.mp3", mp3(&tag(&chain(100_000))));
    assert_reads("few_tables.mp3", mp3(&tag(&chain(40))));
}

#[test]
fn a_shouted_title_is_a_title_behind_a_frame_that_ends_the_trust() {
    // One mention passes on any path, with flags that look compressed included.
    let mut frames = frame(4, b"RVA2", 0, &[0; 4]);
    frames.extend_from_slice(&title(4, "CHAPTER 19 and something"));
    assert_eq!(
        crate::tagio::chapter_nesting::check_tag(&tag(&frames)),
        Ok(())
    );

    // A link in a chapter is read to its end, so it costs a long episode no trust.
    let mut episode = title(4, "Episode");
    for index in 0..40 {
        let mut body = chapter_prefix(&format!("c{index}"), index * 1000);
        body.extend_from_slice(&title(4, &format!("CHAPTER {index}")));
        let mut link = vec![3];
        link.extend_from_slice(b"site\0https://example.com/");
        body.extend_from_slice(&frame(4, b"WXXX", 0, &link));
        episode.extend_from_slice(&frame(4, b"CHAP", 0, &body));
    }
    assert_reads("linked.mp3", mp3(&tag(&episode)));
}

#[test]
fn a_tag_that_declares_less_than_it_holds_gives_the_id3_crate_only_that() {
    use crate::tagio::chapter_nesting::read_id3_tag;

    // That crate reads frames until a count runs out, not to the end of the tag.
    let mut lying = b"ID3\x04\0\0".to_vec();
    lying.extend_from_slice(&synchsafe(1));
    lying.extend_from_slice(&nested(4, &[b"CHAP"], 2_000));
    on_stack(SMALL_STACK, move || {
        let _ = read_id3_tag(&mut Cursor::new(lying));
    });

    // Nor a length with its top bit set, which it reads as more than lofty does.
    let mut high = b"ID3\x04\0\0".to_vec();
    high.extend_from_slice(&[0, 0, 0x80, 0]);
    high.extend_from_slice(&nested(4, &[b"CHAP"], 2_000));
    on_stack(SMALL_STACK, move || {
        let _ = read_id3_tag(&mut Cursor::new(high));
    });

    let good = tag(&title(4, "Kept"));
    on_stack(HONEST_STACK, move || {
        use id3::TagLike;
        let read = read_id3_tag(&mut Cursor::new(good)).expect("a good tag reads");
        assert_eq!(read.title(), Some("Kept"));
    });
    assert!(matches!(
        read_id3_tag(&mut Cursor::new(b"not a tag at all".to_vec())),
        Err(id3::Error {
            kind: id3::ErrorKind::NoTag,
            ..
        })
    ));
}

#[test]
fn silence_behind_a_data_chunk_of_no_length_is_not_a_refusal() {
    let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&[1, 0, 1, 0]);
    wav.extend_from_slice(&8000_u32.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&[2, 0, 16, 0]);
    wav.extend_from_slice(b"data\0\0\0\0");
    wav.extend_from_slice(&vec![0; 1024 * 1024]);
    assert_eq!(
        crate::tagio::chapter_nesting::check_reader(&mut Cursor::new(wav)),
        Ok(())
    );
}

#[test]
fn a_frame_lofty_reads_whole_keeps_a_long_episode_readable() {
    let mut frames = frame(4, b"PCST", 0, &[0; 4]);
    frames.extend_from_slice(&frame(4, b"GRP1", 0, b"\x03Show"));
    frames.extend_from_slice(&episode(4, 40, |i| format!("CHAPTER {i}")));
    assert_reads("pcst.mp3", mp3(&tag(&frames)));
}

#[test]
fn what_an_extended_header_loses_past_the_end_of_the_tag_is_looked_at() {
    // Unsynchronised 2.3: lofty reads the extended header through a stream that
    // buffers 8 KiB and drops it, so the frames it reads start past all of that.
    let mut bytes = b"ID3\x03\0\xc0".to_vec();
    let chain = nested(3, &[b"CHAP"], 64);
    // Enough for the chain, but all of it declared ahead of where the chain really is.
    let declared = 6 + chain.len();
    bytes.extend_from_slice(&synchsafe(declared));
    bytes.extend_from_slice(&[0, 0, 0, 6, 0, 0, 0, 0, 0, 0]);
    // Through `Probe`'s own buffer that is 8 KiB from the start of the file.
    bytes.resize(8192, 0);
    bytes.extend_from_slice(&chain);
    assert_refused_everywhere("extended.mp3", mp3(&bytes));
}

#[test]
fn a_name_cut_by_the_end_of_a_frame_is_still_a_name() {
    // A URL lofty stops at its first zero, so it goes on at the "CH" behind it, with
    // the next frame's id as the rest of the name.
    let mut frames = Vec::new();
    for _ in 0..8 {
        frames.extend_from_slice(&frame(4, b"WOAR", 0, b"x\0CH"));
        frames.extend_from_slice(&frame(4, b"AP00", 0, &[0; 4]));
    }
    assert_eq!(
        crate::tagio::chapter_nesting::check_tag(&tag(&frames)),
        Err(crate::tagio::chapter_nesting::NestedChapters)
    );
}

#[test]
fn a_compressed_body_behind_unsynchronised_lengths_is_refused() {
    // Each 0xff 0x00 of the length moves the stream on by a byte a parser takes out.
    let mut inside = chapter_prefix("c", 0);
    inside.extend_from_slice(&nested(4, &[b"CHAP"], 64));
    let mut child = b"CHAP".to_vec();
    child.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0, 0x09]);
    child.extend_from_slice(&[0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0]);
    child.extend_from_slice(&zlib_stored(&inside));
    let mut body = chapter_prefix("a", 0);
    body.extend_from_slice(&child);
    assert_refused_everywhere(
        "zlib_unsync.mp3",
        mp3(&tag(&frame(4, b"CHAP", 0x0002, &body))),
    );
}

#[test]
fn a_tag_chunk_inside_a_marker_chunk_that_declares_too_much_is_found() {
    // symphonia reads a MARK's count and its markers, and looks for the next chunk right
    // there, where a walk by declared lengths would have jumped past it.
    let hostile = tag(&nested(4, &[b"CHAP"], 2_000));
    let mut inside = vec![0_u8; 2];
    inside.extend_from_slice(b"ID3 ");
    inside.extend_from_slice(&(hostile.len() as u32).to_be_bytes());
    inside.extend_from_slice(&hostile);
    if inside.len() % 2 == 1 {
        inside.push(0);
    }
    let mut body = b"AIFFCOMM".to_vec();
    body.extend_from_slice(&18_u32.to_be_bytes());
    body.extend_from_slice(&1_u16.to_be_bytes());
    body.extend_from_slice(&800_u32.to_be_bytes());
    body.extend_from_slice(&16_u16.to_be_bytes());
    body.extend_from_slice(&[0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    body.extend_from_slice(b"MARK");
    body.extend_from_slice(&(inside.len() as u32).to_be_bytes());
    body.extend_from_slice(&inside);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&1608_u32.to_be_bytes());
    body.extend_from_slice(&[0; 1608]);
    let mut form = b"FORM".to_vec();
    // The reader counts each chunk by what it declares, so the form declares enough.
    form.extend_from_slice(&(body.len() as u32 * 3).to_be_bytes());
    form.extend_from_slice(&body);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), "mark.aiff", &form);
    on_stack(SMALL_STACK, move || {
        let _ = read_tagged_file(&path);
        let _ = probe_playable_stream(&path);
    });
}

#[test]
fn only_bytes_that_are_no_tag_read_as_no_tag() {
    use crate::tagio::chapter_nesting::read_id3_tag;

    // A writer starts afresh on `NoTag`, over a cover and ratings it could not read.
    let future = b"ID3\x05\0\0\0\0\0\x10and sixteen bytes".to_vec();
    let refused = read_id3_tag(&mut Cursor::new(future)).expect_err("nobody reads 2.5");
    assert!(!matches!(refused.kind, id3::ErrorKind::NoTag), "{refused}");

    let hostile = tag(&nested(4, &[b"CHAP"], 64));
    let refused = read_id3_tag(&mut Cursor::new(hostile)).expect_err("nested");
    assert!(!matches!(refused.kind, id3::ErrorKind::NoTag), "{refused}");

    let empty = read_id3_tag(&mut Cursor::new(Vec::new())).expect_err("nothing here");
    assert!(matches!(empty.kind, id3::ErrorKind::NoTag));
}

/// A frame ends the trust, and what follows it is judged by the row: the chapter
/// behind it is a frame of that row, not a name hiding in the frame before it.
#[test]
fn flat_chapters_behind_a_frame_that_ends_the_trust_are_flat() {
    let mut toc = b"toc\0".to_vec();
    toc.extend_from_slice(&[0x03, 8]);
    for index in 0..8 {
        toc.extend_from_slice(format!("c{index}\0").as_bytes());
    }
    let contents = frame(4, b"CTOC", 0, &toc);
    let chapters = episode(4, 8, |i| format!("Chapter {i}"));

    let people = frame(4, b"TIPL", 0, b"\x03producer\0someone\0");
    assert_reads(
        "people.mp3",
        mp3(&tag(&[people, contents.clone(), chapters.clone()].concat())),
    );

    // The flag asks a tagger to drop the frame if the file changes. Nothing else.
    let flagged = frame(4, b"TPE1", 0x4000, b"\x03Someone");
    assert_reads(
        "flagged.mp3",
        mp3(&tag(&[flagged, contents, chapters].concat())),
    );
}

/// A reader that says how much was read from it, and stops a check that has already
/// gone far past anything a good file could ask for.
struct Counted {
    inner: Cursor<Vec<u8>>,
    read: u64,
}

impl Counted {
    /// Twice the budget: well past it, and well before a runaway takes a minute.
    const GONE_TOO_FAR: u64 = 2 * 1024 * 1024 * 1024;

    fn new(bytes: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(bytes),
            read: 0,
        }
    }
}

impl std::io::Read for Counted {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        assert!(self.read < Self::GONE_TOO_FAR, "the check read without end");
        let read = self.inner.read(buffer)?;
        self.read += read as u64;
        Ok(read)
    }
}

impl std::io::Seek for Counted {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

/// A WAV with `body` as its one chunk before the audio.
fn wav_with_list(body: &[u8]) -> Vec<u8> {
    let mut riff = b"WAVELIST".to_vec();
    riff.extend_from_slice(&(body.len() as u32).to_le_bytes());
    riff.extend_from_slice(body);
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(riff.len() as u32).to_le_bytes());
    out.extend_from_slice(&riff);
    out
}

/// The name of a tag chunk that declares more than the file holds, so that reading it
/// reads everything behind it.
fn greedy_chunk_name() -> Vec<u8> {
    [b"ID3 ".as_slice(), &u32::MAX.to_le_bytes()].concat()
}

#[test]
fn the_name_of_a_tag_chunk_inside_another_chunk_costs_nothing() {
    use crate::tagio::chapter_nesting::check_reader;

    // lofty goes by the declared lengths and does not look in there, and symphonia is
    // not shown this chunk at all, so nothing has to read it, and nothing is queued.
    let body = greedy_chunk_name().repeat(200_000);
    let size = body.len() as u64;
    let mut file = Counted::new(wav_with_list(&body));
    assert_eq!(check_reader(&mut file), Ok(()));
    assert!(file.read <= size + 64 * 1024, "{}", file.read);
}

#[test]
fn tags_named_again_and_again_behind_an_ape_tag_do_not_read_the_file_again_and_again() {
    use crate::tagio::chapter_nesting::{check_reader, NestedChapters};

    // Behind an APE tag nothing is jumped over, so each of these is a tag to read. 2.2
    // has no chapters to look for, which leaves the reading.
    let mut bytes = b"APETAGEX".to_vec();
    bytes.resize(32, 0);
    bytes.extend_from_slice(&b"ID3\x02\0\0\x7f\x7f\x7f\x7f".repeat(50_000));
    let mut file = Counted::new(bytes);
    assert_eq!(check_reader(&mut file), Err(NestedChapters));
    assert!(
        file.read <= 1024 * 1024 * 1024 + 1024 * 1024,
        "{}",
        file.read
    );
}

#[test]
fn a_good_tag_behind_an_ape_tag_costs_one_reading_or_so() {
    use crate::tagio::chapter_nesting::check_reader;

    let mut bytes = b"APETAGEX".to_vec();
    bytes.resize(32, 0);
    bytes.extend_from_slice(&tag(&episode(4, 40, |i| format!("Chapter {i}"))));
    bytes.extend_from_slice(&audio());
    let size = bytes.len() as u64;
    let mut file = Counted::new(bytes);
    assert_eq!(check_reader(&mut file), Ok(()));
    assert!(file.read <= 64 * size, "{}", file.read);
}

// Every bound of the check stops it before a parser would stop. Stopping has to mean
// refused, or that no parser goes further: these say which, for each one.

/// An AIFF whose `ID3 ` chunk declares `chunk_len` and holds `tag`.
fn aiff_with_short_tag_chunk(chunk_len: u32, tag: &[u8]) -> Vec<u8> {
    let mut body = b"AIFFCOMM".to_vec();
    body.extend_from_slice(&18_u32.to_be_bytes());
    body.extend_from_slice(&1_u16.to_be_bytes());
    body.extend_from_slice(&800_u32.to_be_bytes());
    body.extend_from_slice(&16_u16.to_be_bytes());
    body.extend_from_slice(&[0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    // Further than the search of the head reads, or it would find the tag by chance.
    body.extend_from_slice(b"JUNK");
    body.extend_from_slice(&100_000_u32.to_be_bytes());
    body.extend_from_slice(&[0; 100_000]);
    body.extend_from_slice(b"ID3 ");
    body.extend_from_slice(&chunk_len.to_be_bytes());
    body.extend_from_slice(tag);
    if tag.len() % 2 == 1 {
        body.push(0);
    }
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&1608_u32.to_be_bytes());
    body.extend_from_slice(&[0; 1608]);
    let mut form = b"FORM".to_vec();
    form.extend_from_slice(&(body.len() as u32 * 3).to_be_bytes());
    form.extend_from_slice(&body);
    form
}

/// symphonia reads the tag in a chunk as far as the tag declares, not the chunk.
#[test]
fn a_tag_that_runs_past_the_end_of_its_chunk_is_read_as_far_as_it_declares() {
    let hostile = tag(&nested(4, &[b"CHAP"], 2_000));
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(
        dir.path(),
        "short_chunk.aiff",
        &aiff_with_short_tag_chunk(10, &hostile),
    );
    on_stack(SMALL_STACK, move || {
        assert!(probe_playable_stream(&path).is_none());
    });
}

/// A reader that stops answering partway, as a share can: a parser after it may be
/// answered, so what was not seen was not passed.
struct FailsAfter {
    inner: Cursor<Vec<u8>>,
    left: usize,
}

impl std::io::Read for FailsAfter {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::ErrorKind::ConnectionReset.into());
        }
        let most = buffer.len().min(self.left);
        let read = self.inner.read(&mut buffer[..most])?;
        self.left -= read;
        Ok(read)
    }
}

impl std::io::Seek for FailsAfter {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

#[test]
fn a_read_that_fails_partway_is_not_a_clean_bill() {
    use crate::tagio::chapter_nesting::{check_reader, NestedChapters};

    let bytes = mp3(&tag(&nested(4, &[b"CHAP"], 64)));
    let mut failing = FailsAfter {
        inner: Cursor::new(bytes),
        left: 100,
    };
    assert_eq!(check_reader(&mut failing), Err(NestedChapters));
}

/// The bound on chunks: past it the file is refused, whatever lies behind.
#[test]
fn a_tag_behind_more_chunks_than_the_walk_allows_is_refused() {
    let mut body = b"AIFF".to_vec();
    for _ in 0..70_000 {
        body.extend_from_slice(b"JUNK\0\0\0\0");
    }
    let hostile = tag(&nested(4, &[b"CHAP"], 64));
    body.extend_from_slice(b"ID3 ");
    body.extend_from_slice(&(hostile.len() as u32).to_be_bytes());
    body.extend_from_slice(&hostile);
    let mut form = b"FORM".to_vec();
    form.extend_from_slice(&(body.len() as u32).to_be_bytes());
    form.extend_from_slice(&body);
    assert_refused_everywhere("many_chunks.aiff", form);
}

/// The window behind the head: past it the check says nothing is there, and that holds
/// only because no parser looks further. lofty stops at the first audio frame or at a
/// kilobyte of junk, for each format it reads a tag of.
#[test]
fn a_tag_past_the_window_reaches_no_parser() {
    use crate::tagio::chapter_nesting::check_file;

    let hostile = tag(&nested(4, &[b"CHAP"], 2_000));
    let junk = vec![0x5a_u8; 70_000];
    let dir = tempfile::tempdir().expect("tempdir");
    for name in [
        "a.mp3", "a.aac", "a.flac", "a.ape", "a.mpc", "a.wv", "a.ogg",
    ] {
        for lead in [junk.clone(), audio().repeat(5)] {
            let bytes = [lead, hostile.clone(), audio()].concat();
            let path = write_in(dir.path(), name, &bytes);
            assert_eq!(check_file(&path), Ok(()), "{name} is past the window");
            on_stack(SMALL_STACK, move || {
                // Refused or read, it does not matter here: dying is the failure.
                let _ = read_tagged_file(&path);
                let _ = read_concrete_tags(&path);
                let _ = probe_playable_stream(&path);
            });
        }
    }
}

/// The versions the check passes unread: 2.2 has no chapter frame, the next one has no
/// parser, and neither gets anyone to recurse.
#[test]
fn a_tag_of_a_version_with_no_chapter_frame_reaches_no_recursion() {
    use crate::tagio::chapter_nesting::{check_tag, read_id3_tag};

    for major in [2_u8, 5] {
        let bytes = tag_with(major, 0, &nested(4, &[b"CHAP"], 2_000));
        assert_eq!(check_tag(&bytes), Ok(()));
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_in(dir.path(), "version.mp3", &mp3(&bytes));
        on_stack(SMALL_STACK, move || {
            let _ = read_tagged_file(&path);
            let _ = read_concrete_tags(&path);
            let _ = probe_playable_stream(&path);
            let _ = read_id3_tag(&mut Cursor::new(bytes));
        });
    }
}

/// A reader interrupted once: nothing failed, so the read is tried again, and the tag
/// behind it is looked at like any other.
struct InterruptedOnce {
    inner: Cursor<Vec<u8>>,
    interrupted: bool,
}

impl std::io::Read for InterruptedOnce {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        self.inner.read(buffer)
    }
}

impl std::io::Seek for InterruptedOnce {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

#[test]
fn a_read_that_is_interrupted_is_tried_again_and_hides_nothing() {
    use crate::tagio::chapter_nesting::{check_reader, NestedChapters};

    let interrupted = |bytes: Vec<u8>| InterruptedOnce {
        inner: Cursor::new(bytes),
        interrupted: false,
    };
    let hostile = mp3(&tag(&nested(4, &[b"CHAP"], 64)));
    assert_eq!(check_reader(&mut interrupted(hostile)), Err(NestedChapters));
    // And it costs a good file nothing.
    let good = mp3(&tag(&title(4, "Kept")));
    assert_eq!(check_reader(&mut interrupted(good)), Ok(()));
}

const AIFF_COMM_BODY: [u8; 18] = [
    0, 2, // channels
    0, 0, 0x03, 0x20, // 800 frames
    0, 16, // bits
    0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0, // 44 100 Hz
];

fn iff_chunk(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut chunk = name.to_vec();
    chunk.extend_from_slice(&(body.len() as u32).to_be_bytes());
    chunk.extend_from_slice(body);
    if body.len() % 2 == 1 {
        chunk.push(0);
    }
    chunk
}

fn form(kind: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let body = [kind.as_slice(), &chunks.concat()].concat();
    let mut out = b"FORM".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// A `SSND` body whose `at`-th byte starts `what`, the rest of it silence.
fn sound_holding(at: usize, what: &[u8]) -> Vec<u8> {
    let mut body = vec![0_u8; at];
    body.extend_from_slice(what);
    body.resize(body.len() + 64, 0);
    body
}

fn tag_chunk_named(hostile: &[u8]) -> Vec<u8> {
    let mut chunk = b"ID3 ".to_vec();
    chunk.extend_from_slice(&(hostile.len() as u32).to_be_bytes());
    chunk.extend_from_slice(hostile);
    chunk
}

/// The audio does not go through a parser: what matters here is that nothing dies.
fn assert_survives(name: &str, bytes: Vec<u8>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_in(dir.path(), name, &bytes);
    on_stack(SMALL_STACK, move || {
        let _ = read_tagged_file(&path);
        let _ = read_concrete_tags(&path);
        let _ = probe_playable_stream(&path);
    });
}

/// symphonia reads the 18 bytes of a `COMM` that declares 26 and takes the rest for a
/// chunk it skips, which lands it inside the audio, on a chunk no walk by the declared
/// lengths ever looks at. Past 64 KiB, or the search of the head would see it by chance.
#[test]
fn a_tag_chunk_in_the_audio_that_a_long_common_chunk_lands_on_is_out_of_reach() {
    const INTO_THE_AUDIO: u32 = 100_000;
    let mut common = AIFF_COMM_BODY.to_vec();
    common.extend_from_slice(b"XXXX");
    common.extend_from_slice(&INTO_THE_AUDIO.to_be_bytes());
    let hostile = tag(&nested(4, &[b"CHAP"], 2_000));
    let sound = sound_holding(INTO_THE_AUDIO as usize, &tag_chunk_named(&hostile));
    assert_survives(
        "long_comm.aiff",
        form(
            b"AIFF",
            &[iff_chunk(b"COMM", &common), iff_chunk(b"SSND", &sound)],
        ),
    );
}

/// A tag chunk of ten bytes, whose tag declares far more and is read that far, so that
/// the reader goes on inside the audio.
#[test]
fn a_tag_that_declares_its_way_into_the_audio_is_out_of_reach() {
    const INTO_THE_AUDIO: usize = 100_000;
    let mut harmless = b"ID3\x04\0\0".to_vec();
    harmless.extend_from_slice(&synchsafe(INTO_THE_AUDIO));
    let hostile = tag(&nested(4, &[b"CHAP"], 2_000));
    // The tag starts 18 bytes before the sound chunk's body and ends 10 past its own
    // header's size: this is where the reader goes on.
    let sound = sound_holding(INTO_THE_AUDIO - 8, &tag_chunk_named(&hostile));
    assert_survives(
        "long_tag.aiff",
        form(
            b"AIFF",
            &[
                iff_chunk(b"COMM", &AIFF_COMM_BODY),
                // Far enough that the search of the head has given up before the tag.
                iff_chunk(b"APPL", &vec![0; 5_000]),
                iff_chunk(b"ID3 ", &harmless),
                iff_chunk(b"SSND", &sound),
            ],
        ),
    );
}

/// What a good AIFF has to keep: the same packets, in every shape of one.
#[test]
fn a_good_aiff_plays_the_same_through_the_guard() {
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatReader, SeekMode, SeekTo};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::units::Timestamp;

    fn packets(format: &mut dyn FormatReader) -> Vec<Vec<u8>> {
        let mut all = Vec::new();
        while let Ok(Some(packet)) = format.next_packet() {
            all.push(packet.data.to_vec());
        }
        all
    }

    let frames: Vec<u8> = (0..1600_u16).flat_map(u16::to_be_bytes).collect();
    let sound = |offset: usize| {
        let mut body = (offset as u32).to_be_bytes().to_vec();
        body.extend_from_slice(&[0; 4]);
        body.resize(body.len() + offset, 0x5a);
        body.extend_from_slice(&frames);
        body
    };
    let mut aifc_common = AIFF_COMM_BODY.to_vec();
    aifc_common.extend_from_slice(b"NONE");
    aifc_common.extend_from_slice(b"\x0enot compressed\0");
    let good_tag = tag(&title(4, "Kept"));

    let shapes = [
        (
            "plain",
            form(
                b"AIFF",
                &[
                    iff_chunk(b"COMM", &AIFF_COMM_BODY),
                    iff_chunk(b"SSND", &sound(0)),
                ],
            ),
        ),
        (
            "sound first",
            form(
                b"AIFF",
                &[
                    iff_chunk(b"SSND", &sound(0)),
                    iff_chunk(b"COMM", &AIFF_COMM_BODY),
                ],
            ),
        ),
        (
            "padded, with text and a tag on either side",
            form(
                b"AIFF",
                &[
                    iff_chunk(b"NAME", b"odd"),
                    iff_chunk(b"COMM", &AIFF_COMM_BODY),
                    iff_chunk(b"ID3 ", &good_tag),
                    iff_chunk(b"SSND", &sound(1)),
                    iff_chunk(b"ANNO", b"behind the audio"),
                ],
            ),
        ),
        (
            "compressed by name only",
            form(
                b"AIFC",
                &[
                    iff_chunk(b"COMM", &aifc_common),
                    iff_chunk(b"SSND", &sound(0)),
                ],
            ),
        ),
    ];
    for (shape, bytes) in shapes {
        let mut direct = symphonia::default::formats::AiffReader::try_new(
            MediaSourceStream::new(Box::new(Cursor::new(bytes.clone())), Default::default()),
            Default::default(),
        )
        .unwrap_or_else(|e| panic!("{shape}, without the guard: {e}"));
        let mut guarded = crate::audio_format::probe()
            .probe(
                &Hint::new(),
                MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default()),
                Default::default(),
                Default::default(),
            )
            .unwrap_or_else(|e| panic!("{shape}, through the guard: {e}"));

        let seek_to = |format: &mut dyn FormatReader| {
            let to = SeekTo::Timestamp {
                ts: Timestamp::from(400_i64),
                track_id: format.tracks()[0].id,
            };
            format.seek(SeekMode::Accurate, to).map(|at| at.actual_ts)
        };
        let expected = packets(&mut direct);
        assert!(!expected.is_empty(), "{shape}");
        assert_eq!(expected, packets(&mut *guarded), "{shape}");
        assert_eq!(
            seek_to(&mut direct).ok(),
            seek_to(&mut *guarded).ok(),
            "{shape}: the seek"
        );
        assert_eq!(
            packets(&mut direct),
            packets(&mut *guarded),
            "{shape}: after it"
        );
    }
}

/// Opening a file on a share or behind a URL fetches what it reads: the audio stays
/// where it is until it is played, which is why the chunks are not searched instead.
#[test]
fn opening_an_aiff_does_not_read_its_audio() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::{MediaSource, MediaSourceStream};

    struct Counting {
        inner: Cursor<Vec<u8>>,
        read: Arc<AtomicU64>,
    }
    impl std::io::Read for Counting {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.read.fetch_add(read as u64, Ordering::Relaxed);
            Ok(read)
        }
    }
    impl std::io::Seek for Counting {
        fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(to)
        }
    }
    impl MediaSource for Counting {
        fn is_seekable(&self) -> bool {
            true
        }
        fn byte_len(&self) -> Option<u64> {
            Some(self.inner.get_ref().len() as u64)
        }
    }

    // The sound chunk's offset and block size, then the audio.
    let mut audio = vec![0_u8; 8];
    audio.resize(64 * 1024 * 1024, 0x11);
    let bytes = form(
        b"AIFF",
        &[
            iff_chunk(b"COMM", &AIFF_COMM_BODY),
            iff_chunk(b"SSND", &audio),
        ],
    );
    let read = Arc::new(AtomicU64::new(0));
    let source = Counting {
        inner: Cursor::new(bytes),
        read: Arc::clone(&read),
    };
    let opened = crate::audio_format::probe().probe(
        &Hint::new(),
        MediaSourceStream::new(Box::new(source), Default::default()),
        Default::default(),
        Default::default(),
    );
    assert!(opened.is_ok());
    assert!(
        read.load(Ordering::Relaxed) < 2 * 1024 * 1024,
        "{} bytes to open",
        read.load(Ordering::Relaxed)
    );
}
