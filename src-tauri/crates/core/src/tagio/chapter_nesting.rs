//! Keeps a tag whose chapter frames nest away from the ID3v2 parsers.
//!
//! lofty, symphonia and the `id3` crate all read the frames inside a `CHAP` or
//! `CTOC` with the function that reads the tag, so a chapter inside a chapter
//! recurses once per level, with no limit and no option for one. Measured on a
//! 2 MiB stack, the size of a blocking task's: lofty dies at 64 levels in a release
//! build, about two kilobytes of file, and a stack overflow ends the process. No
//! caller can catch it, so nothing reads a tag this module has not seen first.
//!
//! A walk that reads frames the way one parser does could be led somewhere else by
//! the bytes: lofty stops inside a chapter at a zero, or partway through a URL frame,
//! and reads what is left as the next frame. So this counts, and does not need to
//! agree with anyone about the structure:
//!
//! A parser descends only into a frame it read the id of, in the bytes, and a frame
//! nested in another lies inside the length that other one declares. Lay the tag's
//! top level out by those lengths: two frames of that row do not contain each other,
//! so a chain of chapters holds one of them at most. Every other link is the name
//! `CHAP` or `CTOC` somewhere else, and counting those bounds the depth, whatever a
//! parser makes of the bytes around them.
//!
//! What is not counted is a name inside the body of a frame every parser reads to its
//! end, as long as every frame before it was one too: a chapter titled "CHAPTER 12"
//! is a title, and no parser can be standing inside it. A frame read only partway
//! ends that trust for the rest of the tag, and the count goes on without it.

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// Chapter frame names counted before a tag is refused, so at most this many levels
/// under the first. Nothing real nests them at all, a table of contents names its
/// chapters instead. Four levels leave room on the smallest stack that reads a tag:
/// measured, lofty survives eight on 1 MiB in a debug build, and four on 256 KiB in a
/// release one.
const ALLOWED_UNEXPLAINED_CHAPTERS: usize = 3;

const TAG_HEADER_LEN: usize = 10;
const FRAME_HEADER_LEN: usize = 10;

/// How far past the start of a file, or the end of a tag, another tag is looked for.
/// lofty looks 1024 bytes into junk before the first audio frame. symphonia's reader
/// is guarded where it reads instead, see `audio_format::chapter_guard`.
const HEAD_WINDOW: u64 = 4096;

/// How much of the head is read at a time.
const HEAD_CHUNK: usize = 64 * 1024;

/// Long enough that a signature cut by the end of a chunk is whole in the next one.
const SIGNATURE_OVERLAP: usize = 8;

/// The chunks walked in a WAV or AIFF. A real one has a handful.
const MAX_CHUNKS: usize = 65_536;

/// The tag was refused: its chapter frames nest, or one of them hides its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedChapters;

impl fmt::Display for NestedChapters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the ID3v2 tag nests chapter frames, refusing to read it")
    }
}

impl std::error::Error for NestedChapters {}

/// Whether the file at `path` is safe to hand to a tag parser. A file that cannot be
/// opened or read passes: the parser is about to meet the same error and report it.
pub fn check_file(path: &Path) -> Result<(), NestedChapters> {
    match std::fs::File::open(path) {
        Ok(mut file) => check_reader(&mut file),
        Err(_) => Ok(()),
    }
}

/// [`check_file`] over a reader, from where it stands, which is where it is left.
pub fn check_reader<R: Read + Seek>(reader: &mut R) -> Result<(), NestedChapters> {
    let Ok(start) = reader.stream_position() else {
        return Ok(());
    };
    let verdict = check_tags_from(reader, start);
    if reader.seek(SeekFrom::Start(start)).is_err() {
        tracing::warn!("could not rewind after looking at a file's tags");
    }
    verdict
}

/// Every tag a parser could find from `start`: behind zeros, behind junk, behind
/// another tag or an APE tag, and in the chunks of a WAV or an AIFF.
fn check_tags_from<R: Read + Seek>(reader: &mut R, start: u64) -> Result<(), NestedChapters> {
    let mut buffer = vec![0_u8; HEAD_CHUNK];
    let mut at = start;
    let mut scan_end = start.saturating_add(HEAD_WINDOW);
    let mut leading_zeros = true;
    while at < scan_end {
        let Ok(read) = read_at(reader, at, &mut buffer) else {
            return Ok(());
        };
        let chunk = &buffer[..read];
        if chunk.len() <= SIGNATURE_OVERLAP {
            return Ok(());
        }
        if leading_zeros {
            match chunk.iter().position(|byte| *byte != 0) {
                Some(first) => {
                    leading_zeros = false;
                    at += first as u64;
                    scan_end = at.saturating_add(HEAD_WINDOW);
                }
                None => {
                    at += chunk.len() as u64;
                    scan_end = at.saturating_add(HEAD_WINDOW);
                }
            }
            continue;
        }
        let Some((offset, signature)) = find_signature(chunk) else {
            at += (chunk.len() - SIGNATURE_OVERLAP) as u64;
            continue;
        };
        let found = at + offset as u64;
        match signature {
            Signature::Id3 => match read_tag(reader, found) {
                Some(tag) => {
                    check_tag(&tag.bytes)?;
                    at = found + tag.span;
                    scan_end = at.saturating_add(HEAD_WINDOW);
                }
                None => at = found + 1,
            },
            // lofty reads an APE tag at the head and goes on behind it, so everything
            // it declares is searched.
            Signature::Ape => {
                let declared = chunk.get(offset + 12..offset + 16).map_or(0, |raw| {
                    u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
                });
                scan_end = scan_end.max(
                    found
                        .saturating_add(u64::from(declared))
                        .saturating_add(32 + HEAD_WINDOW),
                );
                at = found + 8;
            }
            Signature::Riff => {
                check_chunks(reader, found, false)?;
                at = found + 4;
            }
            Signature::Form => {
                check_chunks(reader, found, true)?;
                at = found + 4;
            }
        }
    }
    Ok(())
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, buffer: &mut [u8]) -> io::Result<usize> {
    reader.seek(SeekFrom::Start(at))?;
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    Ok(filled)
}

#[derive(Clone, Copy)]
enum Signature {
    Id3,
    Ape,
    Riff,
    Form,
}

fn find_signature(chunk: &[u8]) -> Option<(usize, Signature)> {
    chunk
        .windows(SIGNATURE_OVERLAP)
        .enumerate()
        .find_map(|(offset, window)| {
            let signature = match window {
                [b'I', b'D', b'3', ..] => Signature::Id3,
                [b'A', b'P', b'E', b'T', b'A', b'G', b'E', b'X'] => Signature::Ape,
                [b'R', b'I', b'F', b'F', ..] => Signature::Riff,
                [b'F', b'O', b'R', b'M', ..] => Signature::Form,
                _ => return None,
            };
            Some((offset, signature))
        })
}

struct ReadTag {
    bytes: Vec<u8>,
    /// From the start of the tag to what follows it, a footer included.
    span: u64,
}

/// The tag at `at`, header and all, as far as the bytes go. `None` when these three
/// letters start no tag a parser would take.
fn read_tag<R: Read + Seek>(reader: &mut R, at: u64) -> Option<ReadTag> {
    let mut header = [0_u8; TAG_HEADER_LEN];
    reader.seek(SeekFrom::Start(at)).ok()?;
    reader.read_exact(&mut header).ok()?;
    if !(2..=4).contains(&header[3]) {
        return None;
    }
    let size = synchsafe(&header[6..10]);
    let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
    let mut bytes = header.to_vec();
    reader
        .by_ref()
        .take(u64::from(size))
        .read_to_end(&mut bytes)
        .ok()?;
    Some(ReadTag {
        bytes,
        span: (TAG_HEADER_LEN as u64) + u64::from(size) + footer,
    })
}

/// The `ID3 ` chunks of the WAV or AIFF at `start`. A chunk of odd length is followed
/// by a padding byte that lofty and symphonia do not agree on when it is not a zero,
/// and both continuations are walked.
fn check_chunks<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    big_endian: bool,
) -> Result<(), NestedChapters> {
    let Ok(len) = reader.seek(SeekFrom::End(0)) else {
        return Ok(());
    };
    let mut pending = vec![start + 12];
    let mut seen = std::collections::HashSet::new();
    while let Some(at) = pending.pop() {
        if !seen.insert(at) {
            continue;
        }
        if seen.len() > MAX_CHUNKS {
            return Err(NestedChapters);
        }
        let mut header = [0_u8; 8];
        if reader.seek(SeekFrom::Start(at)).is_err() || reader.read_exact(&mut header).is_err() {
            continue;
        }
        let raw = [header[4], header[5], header[6], header[7]];
        let size = u64::from(if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        });
        if matches!(&header[..4], b"ID3 " | b"id3 ") {
            let mut body = Vec::new();
            if reader.by_ref().take(size).read_to_end(&mut body).is_ok() {
                check_tag(&body)?;
            }
        }
        let next = at + 8 + size;
        if next < len {
            pending.push(next);
            if size % 2 == 1 {
                pending.push(next + 1);
            }
        }
    }
    Ok(())
}

/// One whole tag, from its `ID3` on, or bytes that are not one and pass.
pub fn check_tag(tag: &[u8]) -> Result<(), NestedChapters> {
    let Some(header) = tag.get(..TAG_HEADER_LEN) else {
        return Ok(());
    };
    let major = header[3];
    // 2.2 has three-letter ids and no chapter frame, and nothing reads a later one.
    if &header[..3] != b"ID3" || !(major == 3 || major == 4) {
        return Ok(());
    }
    let flags = header[5];
    let end = TAG_HEADER_LEN.saturating_add(synchsafe(&header[6..10]) as usize);
    let body = &tag[TAG_HEADER_LEN..end.min(tag.len())];
    let unsynchronised = flags & 0x80 != 0;
    let extended = flags & 0x40 != 0;
    let resynchronised;
    // In 2.3 this covers the bytes of the whole tag, frame headers too.
    let frames = if unsynchronised && major == 3 {
        resynchronised = resynchronise(body);
        resynchronised.as_slice()
    } else {
        body
    };
    let mut audit = Audit {
        major,
        unexplained: 0,
        // An extended header's length is one more thing to disagree on, and in 2.4
        // this flag means each frame's bytes are not what the row below sees.
        in_step: !extended && !(unsynchronised && major == 4),
    };
    audit.walk(frames, Level::Top)
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Top,
    InsideChapter,
}

struct Audit {
    major: u8,
    unexplained: usize,
    /// Every frame so far was read to its end by every parser, so all of them are
    /// standing where this walk is.
    in_step: bool,
}

impl Audit {
    fn walk(&mut self, frames: &[u8], level: Level) -> Result<(), NestedChapters> {
        let mut at = 0;
        while let Some(header) = frames.get(at..at + FRAME_HEADER_LEN) {
            if !is_frame_id(&header[..4]) {
                break;
            }
            let raw = &header[4..8];
            let length = if self.major == 4 {
                synchsafe(raw) as usize
            } else {
                u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize
            };
            let flags = u16::from_be_bytes([header[8], header[9]]);
            let body_start = at + FRAME_HEADER_LEN;
            let Some(body) = frames.get(body_start..body_start.saturating_add(length)) else {
                break;
            };
            let id = &header[..4];
            if is_chapter(id) {
                self.refuse_if_compressed(flags)?;
                match (level, self.in_step && flags == 0, children_of(id, body)) {
                    (Level::Top, true, Some(children)) => {
                        self.walk(children, Level::InsideChapter)?;
                    }
                    (Level::Top, _, _) => {
                        self.in_step = false;
                        self.count_names(body)?;
                    }
                    (Level::InsideChapter, _, _) => {
                        self.in_step = false;
                        self.count(1)?;
                        self.count_names(body)?;
                    }
                }
            } else if !(self.in_step && flags == 0 && read_to_its_end(id)) {
                self.in_step = false;
                self.count_names(body)?;
            }
            at = body_start + length;
        }
        let rest = frames.get(at..).unwrap_or_default();
        // What stops a list inside a chapter leaves the parser partway through it.
        if level == Level::InsideChapter && !rest.is_empty() {
            self.in_step = false;
        }
        self.count_names(rest)
    }

    fn count(&mut self, names: usize) -> Result<(), NestedChapters> {
        self.unexplained += names;
        if self.unexplained > ALLOWED_UNEXPLAINED_CHAPTERS {
            Err(NestedChapters)
        } else {
            Ok(())
        }
    }

    fn count_names(&mut self, bytes: &[u8]) -> Result<(), NestedChapters> {
        for (offset, window) in bytes.windows(4).enumerate() {
            if is_chapter(window) {
                if let Some(flags) = bytes.get(offset + 8..offset + 10) {
                    self.refuse_if_compressed(u16::from_be_bytes([flags[0], flags[1]]))?;
                }
                self.count(1)?;
            }
        }
        Ok(())
    }

    /// lofty and `id3` inflate a compressed frame, and what comes out of it is not in
    /// these bytes to be counted. Nothing compresses a chapter.
    fn refuse_if_compressed(&self, flags: u16) -> Result<(), NestedChapters> {
        let compression = if self.major == 4 { 0x0008 } else { 0x0080 };
        if flags & compression != 0 {
            Err(NestedChapters)
        } else {
            Ok(())
        }
    }
}

fn is_chapter(id: &[u8]) -> bool {
    id == b"CHAP" || id == b"CTOC"
}

/// Four capitals or digits: anything else is padding, or no longer a frame.
fn is_frame_id(id: &[u8]) -> bool {
    id.iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// Frames that no parser leaves partway: lofty reads these to the end or fails, which
/// ends the tag, and symphonia and `id3` take a frame's bytes whole. A URL is not one
/// of them, lofty stops at its first zero, and neither is a frame it has its own
/// layout for without a free-length field at the end, the key-value lists included.
fn read_to_its_end(id: &[u8]) -> bool {
    if matches!(id, b"TIPL" | b"TMCL") {
        return false;
    }
    id[0] == b'T'
        || matches!(
            id,
            b"COMM" | b"USLT" | b"APIC" | b"PRIV" | b"UFID" | b"POPM" | b"GEOB" | b"SYLT"
        )
}

/// The frames inside a chapter or a table of contents, past its own fields. `None`
/// when those fields do not fit, and the body is then only bytes.
fn children_of<'a>(id: &[u8], body: &'a [u8]) -> Option<&'a [u8]> {
    let id_end = body.iter().position(|byte| *byte == 0)? + 1;
    let rest = body.get(id_end..)?;
    if id == b"CHAP" {
        return rest.get(16..);
    }
    let (entries, mut rest) = (*rest.get(1)?, rest.get(2..)?);
    for _ in 0..entries {
        let end = rest.iter().position(|byte| *byte == 0)? + 1;
        rest = rest.get(end..)?;
    }
    Some(rest)
}

/// Seven bits to the byte. The top one is dropped, as all three parsers drop it.
fn synchsafe(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0_u32, |value, byte| (value << 7) | u32::from(byte & 0x7f))
}

fn resynchronise(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut after_ff = false;
    for byte in bytes {
        if !(after_ff && *byte == 0) {
            out.push(*byte);
        }
        after_ff = *byte == 0xff;
    }
    out
}
