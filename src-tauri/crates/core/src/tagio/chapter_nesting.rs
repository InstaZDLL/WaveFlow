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
//!
//! symphonia recurses a second way, on the entries of a table of contents, which name
//! the next table with no nesting in the bytes at all. That depth is the number of
//! tables, so that number has a limit of its own.

use std::fmt;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

/// Chapter frame names counted before a tag is refused, so at most this many levels
/// under the first. Nothing real nests them at all, a table of contents names its
/// chapters instead. Four levels leave room on the smallest stack that reads a tag:
/// measured, lofty survives eight on 1 MiB in a debug build, and four on 256 KiB in a
/// release one.
const ALLOWED_UNEXPLAINED_CHAPTERS: usize = 3;

/// Tables of contents in one tag. A real tag has one, a book in parts a few dozen.
const MAX_TABLES_OF_CONTENTS: usize = 64;

const TAG_HEADER_LEN: usize = 10;
const FRAME_HEADER_LEN: usize = 10;

/// How far past the start of a file, or the end of a tag, another tag is looked for
/// at the least: what is read to look is [`HEAD_CHUNK`], and all of it is looked at.
/// lofty looks 1024 bytes into junk before the first audio frame, and takes a tag
/// that follows another after the ten bytes of a footer. symphonia's reader is
/// guarded where it reads instead, see `audio_format::chapter_guard`.
const HEAD_WINDOW: u64 = 4096;

/// What lofty can lose past a tag's declared end when it has an extended header: in
/// 2.3 an unsynchronised one is read through a stream that buffers 8 KiB and is then
/// dropped, so its frames start that much later and end that much later too.
const EXTENDED_HEADER_SLACK: u64 = 16 * 1024;

/// What is looked through behind a chapter's name for the 0xff of unsynchronisation,
/// past which a zlib stream would not be where its flags say.
const COMPRESSED_BODY_PROBE: usize = 16;

/// How much of the head is read at a time.
const HEAD_CHUNK: usize = 64 * 1024;

/// Long enough that a signature cut by the end of a chunk is whole in the next one.
const SIGNATURE_OVERLAP: usize = 8;

/// The chunks walked in a WAV or AIFF, visited and waiting. A real one has a handful,
/// and silence behind a chunk of no length does not count, however long.
const MAX_CHUNKS: usize = 65_536;

/// What one check may read from one file, however it comes to. A tag takes 256 MiB at
/// the very most and the audio is not read, so a good file stays far under this, the
/// exception being one read to its end for an APE tag or zeros at its head, and over a
/// gigabyte. Only bytes made for it go further, by naming a tag again and again so that
/// each name reads the rest of the file: refused, not read on.
const MAX_SCAN_BYTES: u64 = 1024 * 1024 * 1024;

/// The file was refused: its chapter frames nest, one of them hides its content, or
/// the check could not finish, by cost or by a read that failed. A check that stops
/// before a parser would has not seen what the parser is about to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedChapters;

impl fmt::Display for NestedChapters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "refusing to read the ID3v2 tags: their chapter frames nest, \
             or they could not be looked through",
        )
    }
}

impl std::error::Error for NestedChapters {}

/// Whether the file at `path` is safe to hand to a tag parser. A file that cannot be
/// opened passes, the one way this ends without looking: nothing is known of its bytes,
/// the parser opens it next and reports why it cannot, and that says more than this
/// would about a locked or a missing file.
pub fn check_file(path: &Path) -> Result<(), NestedChapters> {
    match std::fs::File::open(path) {
        Ok(mut file) => check_reader(&mut file),
        Err(_) => Ok(()),
    }
}

/// [`check_file`] over a reader, from where it stands, which is where it is left.
pub fn check_reader<R: Read + Seek>(reader: &mut R) -> Result<(), NestedChapters> {
    let Ok(start) = reader.stream_position() else {
        return Err(NestedChapters);
    };
    let mut budgeted = Budgeted {
        inner: &mut *reader,
        left: MAX_SCAN_BYTES,
        gave_up: false,
    };
    let verdict = check_tags_from(&mut budgeted, start);
    let gave_up = budgeted.gave_up;
    // Left anywhere else, the parser that reads it next starts at the wrong place.
    if reader.seek(SeekFrom::Start(start)).is_err() {
        tracing::warn!("could not rewind after looking at a file's tags");
        return Err(NestedChapters);
    }
    if gave_up {
        return Err(NestedChapters);
    }
    verdict
}

/// A reader that fails once it has given [`MAX_SCAN_BYTES`], and remembers that it, or
/// the reader under it, failed at all. The walk below stops quietly on a read it cannot
/// finish, as it must at the end of a file: this is what tells the two apart, so that
/// no stop on a failed read reads as nothing found.
struct Budgeted<'a, R> {
    inner: &'a mut R,
    left: u64,
    gave_up: bool,
}

impl<R: Read> Read for Budgeted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 && !buffer.is_empty() {
            self.gave_up = true;
            return Err(io::ErrorKind::Other.into());
        }
        let most = buffer
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buffer[..most]).inspect_err(|e| {
            self.gave_up |= e.kind() != io::ErrorKind::Interrupted;
        })?;
        self.left -= read as u64;
        Ok(read)
    }
}

impl<R: Seek> Seek for Budgeted<'_, R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to).inspect_err(|_| self.gave_up = true)
    }
}

/// The tag `reader` stands on, through the `id3` crate and only over the bytes that
/// were checked. That crate reads frames until a count runs out, not until the tag
/// ends: a tag declaring one byte would have it parse whatever follows, unseen here.
/// [`id3::ErrorKind::NoTag`] means these bytes are no tag at all, which a writer takes
/// as leave to start a fresh one, so it is kept for that: a tag of a version nobody
/// reads, or one that could not be read, stays an error of its own.
pub fn read_id3_tag<R: Read + Seek>(reader: &mut R) -> Result<id3::Tag, id3::Error> {
    let at = reader.stream_position()?;
    let mut magic = [0_u8; 3];
    match reader.read_exact(&mut magic) {
        Ok(()) if &magic == b"ID3" => {}
        Ok(()) => return Err(id3::Error::new(id3::ErrorKind::NoTag, "no ID3v2 tag here")),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(id3::Error::new(id3::ErrorKind::NoTag, "no ID3v2 tag here"));
        }
        Err(e) => return Err(e.into()),
    }
    let tag = read_declared(reader, at)?;
    check_tag(&tag).map_err(|refused| {
        id3::Error::new(id3::ErrorKind::UnsupportedFeature, refused.to_string())
    })?;
    id3::Tag::read_from2(Cursor::new(tag))
}

/// Every tag a parser could find from `start`: behind zeros, behind junk, behind
/// another tag or an APE tag, and in the chunks of a WAV or an AIFF.
fn check_tags_from<R: Read + Seek>(reader: &mut R, start: u64) -> Result<(), NestedChapters> {
    let mut buffer = vec![0_u8; HEAD_CHUNK];
    let mut at = start;
    let mut scan_end = start.saturating_add(HEAD_WINDOW);
    let mut leading_zeros = true;
    let mut behind_ape = false;
    // Every name of a form in the head starts a walk, and the walks share what they
    // have seen: the same chunks behind many names are walked once, not once each.
    let mut seen_chunks = std::collections::HashSet::new();
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
                }
                None => at += chunk.len() as u64,
            }
            scan_end = at.saturating_add(HEAD_WINDOW);
            continue;
        }
        let Some((offset, signature)) = find_signature(chunk) else {
            at += (chunk.len() - SIGNATURE_OVERLAP) as u64;
            continue;
        };
        let found = at + offset as u64;
        match signature {
            Signature::Id3 => {
                let tag = read_tag(reader, found);
                if let Some(tag) = &tag {
                    check_tag(tag)?;
                }
                at = match tag {
                    Some(tag) if !behind_ape => {
                        let end = found + tag_span(&tag);
                        scan_end = end.saturating_add(HEAD_WINDOW);
                        end
                    }
                    _ => found + 1,
                };
            }
            // lofty walks an APE tag's items, which is not what its header declares,
            // and goes on behind them. A tag there is rare enough to read the file:
            // from here on nothing is jumped over, an item may hold those three letters.
            Signature::Ape => {
                behind_ape = true;
                scan_end = u64::MAX;
                at = found + 1;
            }
            Signature::Riff => {
                check_chunks(reader, found, false, &mut seen_chunks)?;
                at = found + 1;
            }
            Signature::Form => {
                check_chunks(reader, found, true, &mut seen_chunks)?;
                at = found + 1;
            }
        }
    }
    Ok(())
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, buffer: &mut [u8]) -> io::Result<usize> {
    reader.seek(SeekFrom::Start(at))?;
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            // What `read_exact` and `read_to_end` do by themselves. Nothing failed, so
            // the reader under the check does not count it, and this must not end the walk.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
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
                // By its version too: `ID3 ` is the name of a chunk, and each of those
                // read as a tag would read the rest of this block again to find nothing.
                [b'I', b'D', b'3', 2..=4, ..] => Signature::Id3,
                [b'A', b'P', b'E', b'T', b'A', b'G', b'E', b'X'] => Signature::Ape,
                [b'R', b'I', b'F', b'F', ..] => Signature::Riff,
                [b'F', b'O', b'R', b'M', ..] => Signature::Form,
                _ => return None,
            };
            Some((offset, signature))
        })
}

/// The tag at `at`, header and all, as far as the bytes go. `None` when these three
/// letters start no tag a parser would take.
fn read_tag<R: Read + Seek>(reader: &mut R, at: u64) -> Option<Vec<u8>> {
    let tag = read_declared(reader, at).ok()?;
    (&tag[..3] == b"ID3" && (2..=4).contains(&tag[3])).then_some(tag)
}

/// The ten bytes at `at` and as many behind them as they declare, as far as there are
/// any. A tag with an extended header gets [`EXTENDED_HEADER_SLACK`] more.
fn read_declared<R: Read + Seek>(reader: &mut R, at: u64) -> io::Result<Vec<u8>> {
    let mut header = [0_u8; TAG_HEADER_LEN];
    reader.seek(SeekFrom::Start(at))?;
    reader.read_exact(&mut header)?;
    let slack = if header[5] & 0x40 != 0 {
        EXTENDED_HEADER_SLACK
    } else {
        0
    };
    let mut bytes = header.to_vec();
    reader
        .by_ref()
        .take(u64::from(synchsafe(&header[6..10])) + slack)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// From the start of a tag to what follows it. Not past a footer: lofty goes on right
/// behind the frames of a repeated tag, and a footer is no signature to stop at.
fn tag_span(tag: &[u8]) -> u64 {
    (TAG_HEADER_LEN as u64) + u64::from(synchsafe(&tag[6..10]))
}

/// The `ID3 ` chunks of the WAV or AIFF at `start`, for lofty, which goes by declared
/// lengths. A chunk of odd length is followed by a padding byte that it takes or not
/// by what comes next when that is not a zero, and both continuations are walked. A
/// tag is read as far as it declares, which covers whatever the chunk lets a parser
/// read of it. Those four letters in audio that is no such container, which the form
/// type behind them tells, as it tells every parser, would walk garbage for nothing.
/// symphonia does not go by these lengths, and never sees these chunks: see
/// `audio_format::chapter_guard`.
fn check_chunks<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    big_endian: bool,
    seen: &mut std::collections::HashSet<(u64, bool)>,
) -> Result<(), NestedChapters> {
    let Ok(len) = reader.seek(SeekFrom::End(0)) else {
        return Ok(());
    };
    let mut form = [0_u8; 12];
    if reader.seek(SeekFrom::Start(start)).is_err() || reader.read_exact(&mut form).is_err() {
        return Ok(());
    }
    let known = if big_endian {
        matches!(&form[8..], b"AIFF" | b"AIFC")
    } else {
        &form[8..] == b"WAVE"
    };
    if !known {
        return Ok(());
    }
    let mut buffer = vec![0_u8; HEAD_CHUNK];
    let mut pending = vec![start + 12];
    while let Some(at) = pending.pop() {
        if !seen.insert((at, big_endian)) {
            continue;
        }
        // Counted here too: a walk that queues nothing still adds its first position.
        // The byte budget stops those walks first today, since each one rereads a
        // block of the head; the limit should not rest on that.
        if seen.len() > MAX_CHUNKS {
            return Err(NestedChapters);
        }
        let mut header = [0_u8; 8];
        if reader.seek(SeekFrom::Start(at)).is_err() || reader.read_exact(&mut header).is_err() {
            continue;
        }
        // Silence behind a `data` chunk whose length was never written is a chunk
        // with no name and no length, every eight bytes. Not a reason to give up.
        if header == [0; 8] {
            if let Some(next) = past_zero_chunks(reader, at, &mut buffer) {
                queue_chunk(&mut pending, seen.len(), next)?;
            }
            continue;
        }
        let raw = [header[4], header[5], header[6], header[7]];
        let size = u64::from(if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        });
        let next = at + 8 + size;
        if matches!(&header[..4], b"ID3 " | b"id3 ") {
            if let Ok(tag) = read_declared(reader, at + 8) {
                check_tag(&tag)?;
            }
        }
        if next < len {
            queue_chunk(&mut pending, seen.len(), next)?;
            if size % 2 == 1 {
                queue_chunk(&mut pending, seen.len(), next + 1)?;
            }
        }
    }
    Ok(())
}

/// One more position to walk, unless there are already more than a file has reason for.
fn queue_chunk(pending: &mut Vec<u64>, visited: usize, at: u64) -> Result<(), NestedChapters> {
    if visited + pending.len() >= MAX_CHUNKS {
        return Err(NestedChapters);
    }
    pending.push(at);
    Ok(())
}

/// Where the run of empty chunks at `at` ends, on the eight-byte steps it is made of.
fn past_zero_chunks<R: Read + Seek>(reader: &mut R, at: u64, buffer: &mut [u8]) -> Option<u64> {
    let mut from = at;
    loop {
        let read = read_at(reader, from, buffer).ok()?;
        if read == 0 {
            return None;
        }
        if let Some(first) = buffer[..read].iter().position(|byte| *byte != 0) {
            return Some(at + (from + first as u64 - at) / 8 * 8);
        }
        from += read as u64;
    }
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
    let unsynchronised = flags & 0x80 != 0;
    let extended = flags & 0x40 != 0;
    let end = if extended {
        tag.len()
    } else {
        TAG_HEADER_LEN.saturating_add(synchsafe(&header[6..10]) as usize)
    };
    let body = &tag[TAG_HEADER_LEN..end.min(tag.len())];
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
        tables: 0,
        // An extended header's length is one more thing to disagree on, and in 2.4
        // this flag means each frame's bytes are not what the row below sees.
        in_step: !extended && !(unsynchronised && major == 4),
    };
    // The row may start anywhere: what bounds a chain is that two frames of it do not
    // contain each other, not where it starts. So it starts where the extended header
    // says the frames do, and the names in the header itself are counted like any
    // other bytes off the row.
    let row = if extended {
        extended_header_len(major, frames)
    } else {
        0
    };
    audit.count_names(frames, 0, row)?;
    audit.walk(&frames[row..], Level::Top)
}

/// The length of the extended header at the start of `frames`, as its version spells
/// it: 2.4 counts its own four bytes, synchsafe, and 2.3 counts what follows them.
/// Nothing when it says more than there is, and the row then starts at the header.
fn extended_header_len(major: u8, frames: &[u8]) -> usize {
    let Some(raw) = frames.get(..4) else {
        return 0;
    };
    let len = if major == 4 {
        synchsafe(raw) as usize
    } else {
        // Saturating: on a 32-bit target, four past the largest length wraps to a
        // small one that would pass the check below.
        4_usize.saturating_add(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize)
    };
    if len <= frames.len() {
        len
    } else {
        0
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Top,
    InsideChapter,
}

struct Audit {
    major: u8,
    unexplained: usize,
    tables: usize,
    /// Every frame so far was read to its end by every parser, so all of them are
    /// standing where this walk is.
    in_step: bool,
}

impl Audit {
    fn walk(&mut self, frames: &[u8], level: Level) -> Result<(), NestedChapters> {
        let mut at = 0;
        while let Some(header) = frames.get(at..at + FRAME_HEADER_LEN) {
            let raw = &header[4..8];
            // The `id3` crate keeps the top bit of a 2.4 length where lofty drops it, so
            // from a length like this on, they are not looking at the same row.
            if !is_frame_id(&header[..4]) || (self.major == 4 && raw.iter().any(|b| b & 0x80 != 0))
            {
                break;
            }
            let length = if self.major == 4 {
                synchsafe(raw) as usize
            } else {
                u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize
            };
            let flags = u16::from_be_bytes([header[8], header[9]]);
            let body_start = at + FRAME_HEADER_LEN;
            let body_end = body_start.saturating_add(length);
            let Some(body) = frames.get(body_start..body_end) else {
                break;
            };
            let id = &header[..4];
            if is_chapter(id) {
                if flags & self.compression_flag() != 0 {
                    return Err(NestedChapters);
                }
                if level == Level::Top && id == b"CTOC" {
                    self.tables += 1;
                    if self.tables > MAX_TABLES_OF_CONTENTS {
                        return Err(NestedChapters);
                    }
                }
                match (level, self.in_step && flags == 0, children_of(id, body)) {
                    (Level::Top, true, Some(children)) => {
                        self.walk(children, Level::InsideChapter)?;
                        // A name cut by the end of what was just walked alone.
                        if !self.in_step {
                            let from = body_end.saturating_sub(FRAME_HEADER_LEN - 1);
                            self.count_names(frames, from.max(body_start), body_end)?;
                        }
                    }
                    (Level::Top, _, _) => {
                        self.in_step = false;
                        self.count_names(frames, at + 1, body_end)?;
                    }
                    (Level::InsideChapter, _, _) => {
                        self.in_step = false;
                        self.count(1)?;
                        self.count_names(frames, at + 1, body_end)?;
                    }
                }
            } else if !(self.in_step && flags == 0 && read_to_its_end(id, body)) {
                self.in_step = false;
                self.count_names(frames, at + 1, body_end)?;
            }
            at = body_end;
        }
        // What stops a list inside a chapter leaves the parser partway through it.
        if level == Level::InsideChapter && at < frames.len() {
            self.in_step = false;
        }
        self.count_names(frames, at, frames.len())
    }

    fn count(&mut self, names: usize) -> Result<(), NestedChapters> {
        self.unexplained += names;
        if self.unexplained > ALLOWED_UNEXPLAINED_CHAPTERS {
            Err(NestedChapters)
        } else {
            Ok(())
        }
    }

    /// The names starting from `from` and before `to` in `frames`, wherever they end. A
    /// parser that has lost the row reads straight across the frames it is made of, so
    /// a name may start in one and end in the next, and its flags be further on. `to` is
    /// the end of a frame at the most: what starts there is the next frame of the row,
    /// which the walk reaches and judges for itself.
    fn count_names(&mut self, frames: &[u8], from: usize, to: usize) -> Result<(), NestedChapters> {
        let to = to.min(frames.len());
        let Some(starts) = frames.get(from..to) else {
            return Ok(());
        };
        let names = starts
            .iter()
            .enumerate()
            .filter(|(_, byte)| matches!(byte, b'C'))
            .map(|(offset, _)| from + offset);
        for start in names {
            if !frames.get(start..start + 4).is_some_and(is_chapter) {
                continue;
            }
            if let Some(header) = frames.get(start + 4..start + FRAME_HEADER_LEN) {
                // In a frame that is unsynchronised, parsers read this header with the
                // zero after a 0xff taken out, so its flags are not where they look.
                if header.contains(&0xff) {
                    return Err(NestedChapters);
                }
                let flags = u16::from_be_bytes([header[4], header[5]]);
                let after = &frames[start + FRAME_HEADER_LEN..];
                let probe = &after[..after.len().min(COMPRESSED_BODY_PROBE)];
                if flags & self.compression_flag() != 0
                    && (probe.contains(&0xff) || inflatable_after(after))
                {
                    return Err(NestedChapters);
                }
            }
            self.count(1)?;
        }
        Ok(())
    }

    /// lofty and `id3` inflate a compressed frame, and what comes out of it is not in
    /// these bytes to be counted. Nothing compresses a chapter.
    fn compression_flag(&self) -> u16 {
        if self.major == 4 {
            0x0008
        } else {
            0x0080
        }
    }
}

/// Whether a zlib stream starts where a compressed frame's body can, past the group,
/// encryption and length fields its flags may add. "CHAPTER 19" reads as a compressed
/// frame by its flags, and these two bytes are what tell it from one.
fn inflatable_after(body: &[u8]) -> bool {
    (0..=6).any(|skip| match body.get(skip..skip + 2) {
        Some([cmf, flg]) => cmf & 0x0f == 8 && (u16::from(*cmf) * 256 + u16::from(*flg)) % 31 == 0,
        _ => false,
    })
}

fn is_chapter(id: &[u8]) -> bool {
    id == b"CHAP" || id == b"CTOC"
}

/// Four capitals or digits: anything else is padding, or no longer a frame.
fn is_frame_id(id: &[u8]) -> bool {
    id.iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// Whether no parser can leave this frame partway. symphonia and `id3` take a frame's
/// bytes whole, and lofty reads to the end or fails, which ends the tag, with these
/// exceptions, read off its `parse_content`: it stops a URL at its first zero and a
/// comment's UTF-16 text at its first terminator, and the frames with a layout of
/// their own have no free-length field at the end, the key-value lists included.
fn read_to_its_end(id: &[u8], body: &[u8]) -> bool {
    match id {
        b"TIPL" | b"TMCL" | b"IPLS" | b"RVA2" | b"OWNE" | b"ETCO" => false,
        b"COMM" | b"USLT" => comment_read_to_its_end(body),
        b"WXXX" => true,
        _ if id[0] == b'W' => match body.iter().position(|byte| *byte == 0) {
            Some(zero) => zero + 1 == body.len(),
            None => true,
        },
        _ => true,
    }
}

/// Encoding, language, a terminated description, then the text: read to its end in
/// every encoding but the first UTF-16, where it stops at a terminator of its own.
fn comment_read_to_its_end(body: &[u8]) -> bool {
    if body.first() != Some(&1) {
        return true;
    }
    let units = |from: usize| {
        body.get(from..)
            .unwrap_or_default()
            .chunks_exact(2)
            .position(|pair| pair == [0, 0])
            .map(|unit| from + unit * 2 + 2)
    };
    match units(4) {
        Some(text) => units(text).map_or(true, |end| end == body.len()),
        None => true,
    }
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

/// Seven bits to the byte, the top one dropped, as lofty and symphonia read a length.
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
