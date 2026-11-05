//! `CHAP` frames out of the ID3v2 tag at the start of an MP3.
//!
//! Read by hand, and not through lofty's `ChapterFrame::parse`, which is public
//! and reads a good tag well. It reads the frames inside a chapter with the same
//! function that reads the tag, and that function reads `CHAP` too, so a chapter
//! nested in a chapter recurses once per level. Measured: 840 KB of nesting, well
//! under the bound here, overflows an 8 MiB stack, and a stack overflow takes the
//! whole process with it. These bytes come from a host a feed named.
//!
//! So this is one flat walk. A frame inside a chapter is only ever skipped or read
//! as a title, whatever it calls itself, and no frame that declares compression,
//! encryption or its own unsynchronisation is opened: nothing real uses them on a
//! chapter, and each would be one more way to make small bytes large.

use std::io::{Read, Seek, SeekFrom};

use super::chapters::{clean_title, normalize, Chapter, MAX_CHAPTERS};

const TAG_HEADER_LEN: u64 = 10;
const FRAME_HEADER_LEN: usize = 10;

/// How many frames of a tag are walked. A real tag has dozens; 256 MiB of empty
/// frames would be millions of reads for nothing.
const MAX_FRAMES: usize = 65_536;

/// The bytes of a chapter's own header past its id: two times and two offsets.
const CHAPTER_TIMES_LEN: usize = 16;

/// The longest element id looked for.
const MAX_ELEMENT_ID_LEN: usize = 256;

/// The two versions a `CHAP` frame exists in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    V3,
    V4,
}

impl Version {
    /// The frame flags that mean its body is not what it looks like.
    fn opaque_flags(self) -> u16 {
        match self {
            Self::V3 => 0x00c0,
            Self::V4 => 0x000f,
        }
    }
}

/// The chapters of the tag at the start of `reader`, sorted and valid, or none.
/// Reads at most `budget` bytes of chapter frames, and seeks past everything else
/// (a cover is routinely larger than the rest of the tag together). Stops without
/// complaint where the bytes end, so the start of a stream is as good as a file.
pub fn read_chapters<R: Read + Seek>(reader: &mut R, budget: usize) -> Vec<Chapter> {
    walk(reader, budget).map_or_else(Vec::new, |chapters| normalize(chapters, None))
}

fn walk<R: Read + Seek>(reader: &mut R, mut budget: usize) -> Option<Vec<Chapter>> {
    let mut header = [0_u8; TAG_HEADER_LEN as usize];
    reader.read_exact(&mut header).ok()?;
    if &header[..3] != b"ID3" {
        return None;
    }
    let version = match header[3] {
        3 => Version::V3,
        4 => Version::V4,
        _ => return None,
    };
    let flags = header[5];
    // Unsynchronised as a whole: the bytes it inserted would be read as a time or a
    // title, and in 2.3 every length below would be off by them too. Nothing real does
    // this to a chapter, so it is not undone.
    if flags & 0x80 != 0 {
        return None;
    }
    let size = synchsafe(&header[6..10])?;
    let end = TAG_HEADER_LEN + u64::from(size);
    let mut position = TAG_HEADER_LEN;
    if flags & 0x40 != 0 {
        let mut raw = [0_u8; 4];
        reader.read_exact(&mut raw).ok()?;
        position += match version {
            Version::V3 => u64::from(u32::from_be_bytes(raw)) + 4,
            Version::V4 => u64::from(synchsafe(&raw)?),
        };
        reader.seek(SeekFrom::Start(position)).ok()?;
    }

    let mut chapters = Vec::new();
    for _ in 0..MAX_FRAMES {
        if position + FRAME_HEADER_LEN as u64 > end {
            break;
        }
        let mut frame = [0_u8; FRAME_HEADER_LEN];
        if reader.read_exact(&mut frame).is_err() || !is_frame_id(&frame[..4]) {
            // Padding, the end of the bytes, or something that is no frame: nothing
            // further can be located.
            break;
        }
        let after_header = position + FRAME_HEADER_LEN as u64;
        let length = u64::from(frame_length(reader, &frame, version, after_header));
        if after_header + length > end {
            break;
        }
        let frame_flags = u16::from_be_bytes([frame[8], frame[9]]);
        if &frame[..4] == b"CHAP"
            && frame_flags & version.opaque_flags() == 0
            && length as usize <= budget
        {
            let mut body = vec![0; length as usize];
            if reader
                .seek(SeekFrom::Start(after_header))
                .and_then(|_| reader.read_exact(&mut body))
                .is_err()
            {
                break;
            }
            budget -= body.len();
            if let Some(chapter) = read_chapter(&body, version) {
                chapters.push(chapter);
            }
            if chapters.len() >= MAX_CHAPTERS * 2 {
                break;
            }
        }
        position = after_header + length;
        if reader.seek(SeekFrom::Start(position)).is_err() {
            break;
        }
    }
    Some(chapters)
}

/// One `CHAP` frame's body: an id, two times, two offsets, then frames, of which
/// the first non-empty `TIT2` is the title. Walked flat, on purpose (see the module
/// doc): a chapter inside this one is skipped like any other frame.
fn read_chapter(body: &[u8], version: Version) -> Option<Chapter> {
    let id_end = body
        .iter()
        .take(MAX_ELEMENT_ID_LEN)
        .position(|byte| *byte == 0)?;
    let times_start = id_end + 1;
    let times = body.get(times_start..times_start + CHAPTER_TIMES_LEN)?;
    let start_ms = u64::from(u32::from_be_bytes([times[0], times[1], times[2], times[3]]));

    let mut rest = &body[times_start + CHAPTER_TIMES_LEN..];
    let mut title = None;
    while rest.len() >= FRAME_HEADER_LEN && is_frame_id(&rest[..4]) {
        let header = &rest[..FRAME_HEADER_LEN];
        let raw_length: [u8; 4] = header[4..8].try_into().ok()?;
        let length = match version {
            Version::V3 => u32::from_be_bytes(raw_length),
            Version::V4 => synchsafe(&raw_length).unwrap_or_else(|| u32::from_be_bytes(raw_length)),
        } as usize;
        let Some(frame) = rest.get(FRAME_HEADER_LEN..FRAME_HEADER_LEN + length) else {
            break;
        };
        let frame_flags = u16::from_be_bytes([header[8], header[9]]);
        if &header[..4] == b"TIT2" && frame_flags & version.opaque_flags() == 0 {
            let text = clean_title(&decode_text(frame));
            if !text.is_empty() {
                title = Some(text);
                break;
            }
        }
        rest = &rest[FRAME_HEADER_LEN + length..];
    }
    Some(Chapter {
        start_ms,
        title: title?,
    })
}

/// A text frame's first value, by the encoding its first byte names.
fn decode_text(frame: &[u8]) -> String {
    let Some((&encoding, text)) = frame.split_first() else {
        return String::new();
    };
    match encoding {
        0 => text
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| char::from(*byte))
            .collect(),
        1 => match text {
            [0xff, 0xfe, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
            [0xfe, 0xff, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
            // The byte-order mark is required here, so this is a guess, and most
            // tools that forget it write little-endian.
            rest => decode_utf16(rest, u16::from_le_bytes),
        },
        2 => decode_utf16(text, u16::from_be_bytes),
        3 => {
            let end = text
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(text.len());
            String::from_utf8_lossy(&text[..end]).into_owned()
        }
        _ => String::new(),
    }
}

fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| unit([pair[0], pair[1]]))
        .take_while(|value| *value != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// How long a frame says it is. 2.4 writes it synchsafe, and enough tools write it
/// as a plain number that a value which reads differently each way is settled by
/// which of the two lands on another frame.
fn frame_length<R: Read + Seek>(
    reader: &mut R,
    frame: &[u8; FRAME_HEADER_LEN],
    version: Version,
    after_header: u64,
) -> u32 {
    let raw = [frame[4], frame[5], frame[6], frame[7]];
    let plain = u32::from_be_bytes(raw);
    if version == Version::V3 {
        return plain;
    }
    let Some(safe) = synchsafe(&raw) else {
        return plain;
    };
    if safe == plain || lands_on_frame(reader, after_header + u64::from(safe)) {
        return safe;
    }
    if lands_on_frame(reader, after_header + u64::from(plain)) {
        plain
    } else {
        safe
    }
}

fn lands_on_frame<R: Read + Seek>(reader: &mut R, at: u64) -> bool {
    let mut id = [0_u8; 4];
    reader.seek(SeekFrom::Start(at)).is_ok()
        && reader.read_exact(&mut id).is_ok()
        && (is_frame_id(&id) || id[0] == 0)
}

/// Four capitals or digits. A first byte of zero is padding, and anything else is
/// not a frame.
fn is_frame_id(id: &[u8]) -> bool {
    id.len() == 4
        && id
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// Seven bits to the byte, or nothing if a byte has its top bit set.
fn synchsafe(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        (*byte < 0x80).then(|| (value << 7) | u32::from(*byte))
    })
}

/// Whether an enclosure can hold such a tag, by what the feed says of it: worth one
/// request for an MP3 and for a type that says nothing, not for another format. A
/// feed gets a type wrong in both directions, so an odd one falls back on the name.
pub fn may_carry_tag(declared: Option<&str>, url: &url::Url) -> bool {
    let named_mp3 = url.path().to_ascii_lowercase().ends_with(".mp3");
    let Some(declared) = declared else {
        return true;
    };
    let mime = declared
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" | "audio/x-mp3" => true,
        "" | "application/octet-stream" | "binary/octet-stream" | "audio/*" => named_mp3,
        _ => false,
    }
}
