//! Chapters out of an ID3v2 tag, and what a cache is trusted with.

use std::io::Cursor;

use url::Url;

use super::chapters::{self, Chapter, ChapterSource};
use super::id3::{may_carry_tag, read_chapters};
use super::transcript::{self, TranscriptKind, TranscriptRef};
use super::MAX_CHAPTERS_BYTES;

fn synchsafe(value: usize) -> [u8; 4] {
    [
        ((value >> 21) & 0x7f) as u8,
        ((value >> 14) & 0x7f) as u8,
        ((value >> 7) & 0x7f) as u8,
        (value & 0x7f) as u8,
    ]
}

/// A 2.4 frame, its length synchsafe.
fn frame(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    flagged(id, 0, body)
}

fn flagged(id: &[u8; 4], flags: u16, body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&synchsafe(body.len()));
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn title_frame(title: &str) -> Vec<u8> {
    let mut body = vec![3_u8];
    body.extend_from_slice(title.as_bytes());
    frame(b"TIT2", &body)
}

fn chapter_body(id: &str, start_ms: u32, children: &[u8]) -> Vec<u8> {
    let mut body = id.as_bytes().to_vec();
    body.push(0);
    body.extend_from_slice(&start_ms.to_be_bytes());
    body.extend_from_slice(&(start_ms + 1000).to_be_bytes());
    body.extend_from_slice(&[0xff; 8]);
    body.extend_from_slice(children);
    body
}

fn chapter_frame(id: &str, start_ms: u32, title: &str) -> Vec<u8> {
    frame(b"CHAP", &chapter_body(id, start_ms, &title_frame(title)))
}

fn tag(major: u8, flags: u8, frames: &[u8]) -> Vec<u8> {
    let mut out = b"ID3".to_vec();
    out.extend_from_slice(&[major, 0, flags]);
    out.extend_from_slice(&synchsafe(frames.len()));
    out.extend_from_slice(frames);
    out
}

fn read(bytes: &[u8]) -> Vec<Chapter> {
    read_chapters(&mut Cursor::new(bytes), MAX_CHAPTERS_BYTES)
}

fn starts_and_titles(chapters: &[Chapter]) -> Vec<(u64, &str)> {
    chapters
        .iter()
        .map(|chapter| (chapter.start_ms, chapter.title.as_str()))
        .collect()
}

#[test]
fn chapters_are_read_from_either_version_and_sorted() {
    let mut frames = chapter_frame("c2", 60_000, "Second");
    frames.extend(chapter_frame("c1", 0, "First"));
    assert_eq!(
        starts_and_titles(&read(&tag(4, 0, &frames))),
        [(0, "First"), (60_000, "Second")]
    );

    // 2.3 writes a frame's length as a plain number.
    let mut plain = b"CHAP".to_vec();
    let body = {
        let mut title = b"TIT2".to_vec();
        title.extend_from_slice(&5_u32.to_be_bytes());
        title.extend_from_slice(&[0, 0, 0]);
        title.extend_from_slice(b"Old1");
        chapter_body("c", 1000, &title)
    };
    plain.extend_from_slice(&(body.len() as u32).to_be_bytes());
    plain.extend_from_slice(&[0, 0]);
    plain.extend_from_slice(&body);
    assert_eq!(
        starts_and_titles(&read(&tag(3, 0, &plain))),
        [(1000, "Old1")]
    );
}

#[test]
fn a_title_is_read_in_every_encoding_a_tag_may_use() {
    let utf16 = |encoding: u8, mark: &[u8], le: bool, text: &str| {
        let mut body = vec![encoding];
        body.extend_from_slice(mark);
        for unit in text.encode_utf16() {
            body.extend_from_slice(&if le {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        // A terminator, and a second value that must not be read.
        body.extend_from_slice(&[0, 0]);
        frame(b"TIT2", &body)
    };
    let cases = [
        frame(b"TIT2", b"\0caf\xe9\0ignored"),
        utf16(1, &[0xff, 0xfe], true, "café"),
        utf16(1, &[0xfe, 0xff], false, "café"),
        utf16(2, &[], false, "café"),
        frame(b"TIT2", "\u{3}café\0ignored".as_bytes()),
    ];
    for child in cases {
        let bytes = tag(4, 0, &frame(b"CHAP", &chapter_body("c", 0, &child)));
        assert_eq!(starts_and_titles(&read(&bytes)), [(0, "café")]);
    }
}

#[test]
fn a_chapter_nested_as_deep_as_the_bound_allows_does_not_reach_the_stack() {
    // 840 KB of this overflows an 8 MiB stack through lofty's `ChapterFrame::parse`,
    // and a stack overflow is the end of the process. Here, on a stack 32 times
    // smaller.
    let mut inner = title_frame("deep");
    while inner.len() < MAX_CHAPTERS_BYTES - 64 {
        inner = frame(b"CHAP", &chapter_body("c", 0, &inner));
    }
    let bytes = tag(4, 0, &inner);
    let joined = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || read(&bytes))
        .unwrap()
        .join();
    // No title at its own level, and a chapter inside is never gone into.
    assert_eq!(joined.unwrap(), Vec::new());
}

#[test]
fn only_a_valid_title_and_time_make_a_chapter() {
    let mut frames = chapter_frame("ok", 1000, "Kept");
    // No title.
    frames.extend(frame(b"CHAP", &chapter_body("none", 2000, &[])));
    // A title of nothing.
    frames.extend(chapter_frame("blank", 3000, "   "));
    // Past a hundred hours.
    frames.extend(chapter_frame("late", u32::MAX - 2000, "Late"));
    // Cut before its times.
    frames.extend(frame(b"CHAP", b"short\0\x01"));
    // No terminator on its id.
    frames.extend(frame(b"CHAP", &[b'a'; 400]));
    // Markup in a title is no markup.
    frames.extend(chapter_frame("html", 4000, "<b>Bold</b> &amp; loud"));
    assert_eq!(
        starts_and_titles(&read(&tag(4, 0, &frames))),
        [(1000, "Kept"), (4000, "Bold & loud")]
    );
}

#[test]
fn a_frame_that_hides_its_body_is_not_opened() {
    // Compressed and encrypted, whose bodies are not what they look like, and one
    // with a title compressed inside a readable chapter.
    let mut frames = flagged(
        b"CHAP",
        0x0008,
        &chapter_body("zip", 0, &title_frame("Zipped")),
    );
    frames.extend(flagged(
        b"CHAP",
        0x0004,
        &chapter_body("enc", 1000, &title_frame("Sealed")),
    ));
    let mut hidden_title = vec![3_u8];
    hidden_title.extend_from_slice(b"Hidden");
    frames.extend(frame(
        b"CHAP",
        &chapter_body("in", 2000, &flagged(b"TIT2", 0x0008, &hidden_title)),
    ));
    frames.extend(chapter_frame("fine", 3000, "Fine"));
    assert_eq!(
        starts_and_titles(&read(&tag(4, 0, &frames))),
        [(3000, "Fine")]
    );
}

#[test]
fn a_length_written_as_a_plain_number_in_a_2_4_tag_is_settled_by_the_next_frame() {
    let body = chapter_body("c", 500, &title_frame(&"t".repeat(300)));
    assert!(body.len() > 0x7f, "must read differently each way");
    let mut frames = b"CHAP".to_vec();
    frames.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frames.extend_from_slice(&[0, 0]);
    frames.extend_from_slice(&body);
    frames.extend(chapter_frame("next", 9000, "Next"));
    let chapters = read(&tag(4, 0, &frames));
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[1].title, "Next");
}

#[test]
fn the_start_of_a_stream_gives_what_it_holds_and_a_file_seeks_past_a_cover() {
    let mut frames = chapter_frame("a", 0, "Before");
    frames.extend(frame(b"APIC", &vec![0_u8; 3 * MAX_CHAPTERS_BYTES]));
    frames.extend(chapter_frame("b", 1000, "After"));
    let bytes = tag(4, 0, &frames);

    // A file: the cover is seeked past, and nothing of it is read.
    assert_eq!(
        starts_and_titles(&read(&bytes)),
        [(0, "Before"), (1000, "After")]
    );

    // The start of a stream ends in the middle of the cover.
    assert_eq!(
        starts_and_titles(&read(&bytes[..MAX_CHAPTERS_BYTES])),
        [(0, "Before")]
    );

    // A chapter cut by where the bytes end is no chapter, and no panic.
    let whole = tag(4, 0, &chapter_frame("c", 0, "Whole"));
    for end in 0..whole.len() {
        assert_eq!(read(&whole[..end]), Vec::new(), "cut at {end}");
    }
}

#[test]
fn chapter_frames_are_read_up_to_the_budget() {
    let frames: Vec<u8> = (0..100)
        .flat_map(|index| chapter_frame("c", index * 1000, "Chapter"))
        .collect();
    let bytes = tag(4, 0, &frames);
    let frame_len = chapter_frame("c", 0, "Chapter").len();
    let chapters = read_chapters(&mut Cursor::new(&bytes), frame_len * 3);
    assert_eq!(chapters.len(), 3);
}

#[test]
fn what_is_no_tag_gives_nothing() {
    let mut empty_frames = Vec::new();
    for bytes in [
        &b""[..],
        b"ID",
        b"not a tag at all, however long it goes on",
        // A version with no `CHAP`.
        b"ID3\x02\0\0\0\0\0\x10abcdefghijklmnop",
        // Lengths with the top bit set in the tag's own.
        b"ID3\x04\0\0\xff\xff\xff\xff",
    ] {
        assert_eq!(read(bytes), Vec::new());
    }
    // Unsynchronised as a whole, in either version.
    let frames = chapter_frame("c", 0, "Title");
    assert_eq!(read(&tag(3, 0x80, &frames)), Vec::new());
    assert_eq!(read(&tag(4, 0x80, &frames)), Vec::new());
    // Padding ends the walk. So does a million empty frames, with nothing to read.
    assert_eq!(read(&tag(4, 0, &[0_u8; 4096])), Vec::new());
    for _ in 0..100_000 {
        empty_frames.extend(frame(b"TXXX", &[]));
    }
    assert_eq!(read(&tag(4, 0, &empty_frames)), Vec::new());
    // A frame longer than its tag.
    let mut lying = b"CHAP".to_vec();
    lying.extend_from_slice(&synchsafe(100_000));
    lying.extend_from_slice(&[0, 0]);
    assert_eq!(read(&tag(4, 0, &lying)), Vec::new());
}

#[test]
fn an_extended_header_is_stepped_over() {
    let mut frames = vec![0, 0, 0, 6, 1, 0];
    frames.extend(chapter_frame("c", 0, "Title"));
    assert_eq!(
        starts_and_titles(&read(&tag(4, 0x40, &frames))),
        [(0, "Title")]
    );
}

#[test]
fn a_request_for_a_tag_is_made_only_for_what_can_hold_one() {
    let url = |path: &str| Url::parse(&format!("https://cdn.example.org/{path}")).unwrap();
    for (declared, path, expected) in [
        (Some("audio/mpeg"), "show", true),
        (Some("AUDIO/MP3; charset=x"), "show", true),
        (None, "show", true),
        (Some("application/octet-stream"), "a.MP3", true),
        (Some("application/octet-stream"), "a.m4a", false),
        (Some("audio/mp4"), "a.mp3", false),
        (Some("audio/x-m4a"), "a.m4a", false),
        (Some("audio/ogg"), "a.ogg", false),
        (Some("audio/flac"), "a.flac", false),
    ] {
        assert_eq!(
            may_carry_tag(declared, &url(path)),
            expected,
            "{declared:?} {path}"
        );
    }
}

// ── what a cache is trusted with ────────────────────────────────────

fn chapter(start_ms: u64, title: &str) -> Chapter {
    Chapter {
        start_ms,
        title: title.to_owned(),
    }
}

#[test]
fn a_kept_list_comes_back_as_it_went_in() {
    let list = [chapter(0, "One"), chapter(1000, "Two")];
    let stored = chapters::write_cache(ChapterSource::Id3, &list).unwrap();
    assert_eq!(
        chapters::read_cache(&stored),
        Some((ChapterSource::Id3, list.to_vec()))
    );
}

#[test]
fn a_cache_that_is_no_list_is_to_be_found_again() {
    for stored in [
        "",
        "   ",
        "null",
        "{}",
        "[]",
        "not json",
        r#"{"source":"json","chapters":[]}"#,
        r#"{"source":"somewhere","chapters":[{"start_ms":0,"title":"x"}]}"#,
        // Nothing valid in it.
        r#"{"source":"json","chapters":[{"start_ms":0,"title":""}]}"#,
        r#"{"source":"json","chapters":[{"start_ms":99999999999999,"title":"x"}]}"#,
        r#"{"source":"json","chapters":[{"start_ms":-1,"title":"x"}]}"#,
    ] {
        assert_eq!(chapters::read_cache(stored), None, "{stored}");
    }
    let huge = format!(
        r#"{{"source":"json","chapters":[{{"start_ms":0,"title":"{}"}}]}}"#,
        "x".repeat(5 * 1024 * 1024)
    );
    assert_eq!(chapters::read_cache(&huge), None);
}

#[test]
fn a_cache_is_put_back_in_order_and_cut_to_size() {
    let long = "x".repeat(10_000);
    let stored = format!(
        r#"{{"source":"json","chapters":[
            {{"start_ms":5000,"title":"Late"}},
            {{"start_ms":0,"title":"{long}"}},
            {{"start_ms":5000,"title":"Same time"}},
            {{"start_ms":99999999999,"title":"Impossible"}}
        ]}}"#
    );
    let (source, list) = chapters::read_cache(&stored).unwrap();
    assert_eq!(source, ChapterSource::Json);
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].start_ms, 0);
    assert_eq!(list[0].title.chars().count(), 300);
    assert_eq!(list[1].title, "Late");
}

#[test]
fn stored_inline_chapters_are_read_as_hostile_too() {
    assert_eq!(chapters::read_stored("not json"), Vec::new());
    assert_eq!(
        chapters::read_stored(
            r#"[{"start_ms":10,"title":"B"},{"start_ms":0,"title":"A"},{"start_ms":5,"title":""}]"#
        ),
        vec![chapter(0, "A"), chapter(10, "B")]
    );
}

fn reference(kind: TranscriptKind, url: &str) -> TranscriptRef {
    TranscriptRef {
        url: url.to_owned(),
        kind,
        language: None,
    }
}

#[test]
fn transcripts_are_tried_best_first_and_as_the_feed_gave_them() {
    let refs = [
        reference(TranscriptKind::Html, "https://a/h"),
        reference(TranscriptKind::Srt, "https://a/s1"),
        reference(TranscriptKind::Vtt, "https://a/v"),
        reference(TranscriptKind::Srt, "https://a/s2"),
        reference(TranscriptKind::TranscriptJson, "https://a/j"),
        reference(TranscriptKind::Text, "https://a/t"),
    ];
    let order: Vec<&str> = transcript::ranked(&refs)
        .iter()
        .map(|t| t.url.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(order, ["v", "s1", "s2", "j", "h", "t"]);
    assert_eq!(transcript::choose(&refs).unwrap().url, "https://a/v");
}

#[test]
fn a_kind_is_written_and_read_back_and_an_unknown_one_is_none() {
    for kind in [
        TranscriptKind::Vtt,
        TranscriptKind::Srt,
        TranscriptKind::TranscriptJson,
        TranscriptKind::Text,
        TranscriptKind::Html,
    ] {
        assert_eq!(TranscriptKind::from_stored(kind.stored()), Some(kind));
    }
    assert_eq!(TranscriptKind::from_stored("rtf"), None);
    assert_eq!(TranscriptKind::from_stored(""), None);
}

#[test]
fn refs_that_are_not_ours_are_no_refs() {
    assert_eq!(transcript::read_stored_refs("not json"), Vec::new());
    assert_eq!(transcript::read_stored_refs("{}"), Vec::new());
    let stored = serde_json::to_string(&[reference(TranscriptKind::Vtt, "https://a/v")]).unwrap();
    assert_eq!(transcript::read_stored_refs(&stored).len(), 1);
}

#[test]
fn the_panel_gets_text_times_and_speakers_and_no_markup() {
    let vtt = "WEBVTT\n\n00:02.000 --> 00:03.000\n<v Ana>Second <b>line</b>\n\n\
               00:01.000 --> 00:02.000\n<v Bob><script>alert(1)</script>First &amp; only\n";
    let lines =
        transcript::lines_of(&transcript::read(TranscriptKind::Vtt, vtt.as_bytes()).unwrap());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].start_ms, Some(1000));
    assert_eq!(lines[0].speaker.as_deref(), Some("Bob"));
    assert_eq!(lines[1].speaker.as_deref(), Some("Ana"));
    assert_eq!(lines[1].text, "Second line");
    assert_eq!(lines[1].end_ms, Some(3000));
    for line in &lines {
        assert!(!line.text.contains('<'), "markup in {:?}", line.text);
    }
}

#[test]
fn a_page_passed_off_as_a_transcript_is_refused_and_a_declared_page_loses_its_tags() {
    let page = b"<!DOCTYPE html><html><body><h1>Not found</h1><script>x()</script></body></html>";
    assert!(transcript::read(TranscriptKind::Vtt, page).is_err());
    assert!(transcript::read(TranscriptKind::Srt, page).is_err());
    assert!(transcript::read(TranscriptKind::TranscriptJson, page).is_err());
    assert!(transcript::read(TranscriptKind::Text, page).is_err());
    let declared = transcript::read(TranscriptKind::Html, page).unwrap();
    let lines = transcript::lines_of(&declared);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "Not found");
    assert_eq!(lines[0].start_ms, None);
}

#[test]
fn a_kept_transcript_is_read_like_one_just_fetched() {
    let vtt = "WEBVTT\n\n00:01.000 --> 00:02.000\nHello\n";
    let document = transcript::read_cache(TranscriptKind::Vtt, vtt).unwrap();
    assert_eq!(transcript::lines_of(&document)[0].text, "Hello");
    // Something else in the column: refused, not shown.
    assert!(transcript::read_cache(TranscriptKind::Vtt, "<html>").is_err());
    assert!(transcript::read_cache(TranscriptKind::Srt, "garbage").is_err());
    // A kept text larger than a body may grow to.
    let huge = format!("WEBVTT\n\n{}", "x".repeat(16 * 1024 * 1024));
    assert!(transcript::read_cache(TranscriptKind::Vtt, &huge).is_err());
    // Text in a single-byte encoding grows when kept, and is still read back.
    let latin: Vec<u8> = std::iter::repeat(0xe9_u8)
        .take(4 * 1024 * 1024)
        .chain(b"\nline\n".iter().copied())
        .collect();
    let kept = transcript::decode(&latin);
    assert!(kept.len() > latin.len());
    assert!(transcript::read_cache(TranscriptKind::Text, &kept).is_ok());
    // The widest there is: every byte of a body under the bound becomes three, or a
    // kept transcript would be refused at each opening and asked for again.
    let thai = vec![0xa1_u8; 4 * 1024 * 1024];
    let kept = encoding_rs::WINDOWS_874
        .decode_without_bom_handling(&thai)
        .0;
    assert_eq!(kept.len(), thai.len() * 3);
    assert!(transcript::read_cache(TranscriptKind::Text, &kept).is_ok());
}
