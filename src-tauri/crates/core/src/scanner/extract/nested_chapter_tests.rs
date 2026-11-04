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
/// stored blocks, then the checksum.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(0xffff).peekable();
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
