//! An ID3v2 tag written in place, inside the padding the file already has.

use std::io;

/// The ID3v2 tag already on disk, as a span of bytes at the head of the
/// file (#590).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id3v2Span {
    /// Everything the tag occupies: the ten-byte header, the frames, any
    /// padding already there, and the footer when there is one. This is
    /// the room a replacement has to fit into to avoid moving audio.
    pub total: u32,
    /// A footer forbids padding, so a tag that has one cannot be resized
    /// in place — see [`fit_id3v2_padding`].
    pub has_footer: bool,
}

/// Read the ID3v2 header at the very start of `file`, if there is one.
///
/// Deliberately strict about the offset: lofty tolerates junk before the
/// tag, and a tag that does not start at byte zero is one whose span we
/// cannot treat as a prefix to overwrite.
pub fn read_id3v2_span(file: &mut std::fs::File) -> io::Result<Option<Id3v2Span>> {
    use std::io::{Read, Seek, SeekFrom};

    file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 10];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        // A file shorter than a header has no tag, which is an answer,
        // not a failure.
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &header[0..3] != b"ID3" {
        return Ok(None);
    }
    // The size is synchsafe: four bytes of seven bits each, so the high
    // bit never forms a false MPEG sync. A byte with its high bit set is
    // not a valid synchsafe digit and means this is not a header we
    // understand well enough to overwrite.
    let mut size: u32 = 0;
    for byte in &header[6..10] {
        if byte & 0x80 != 0 {
            return Ok(None);
        }
        size = (size << 7) | u32::from(*byte);
    }
    let has_footer = header[5] & 0x10 != 0;
    // The declared size covers neither the header nor the footer.
    let total = size
        .checked_add(if has_footer { 20 } else { 10 })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ID3v2 size overflows"))?;
    Ok(Some(Id3v2Span { total, has_footer }))
}

/// How much padding to ask for so a re-encoded tag lands on exactly the
/// span the old one occupied — `None` when it cannot (#590).
///
/// This is the whole of the fast path's arithmetic. ID3v2 declares its
/// own length and tolerates trailing zeros inside it, so a tag that is
/// no larger than the one already there can be padded back out to the
/// same length and written straight over it. Not one audio byte moves,
/// and the file is never truncated.
///
/// `None` in three cases, each a real one:
/// - there is no tag on disk yet, so there is no span to reuse;
/// - the new tag is bigger than the old span, which is the case the
///   rewrite exists for;
/// - the old tag has a footer, and the spec forbids padding a tag that
///   has one, so the padded re-encode would not be the size we asked
///   for.
pub fn fit_id3v2_padding(existing: Option<Id3v2Span>, encoded_len: u32) -> Option<u32> {
    let span = existing?;
    if span.has_footer {
        return None;
    }
    span.total.checked_sub(encoded_len)
}

/// Write `tag` over the ID3v2 tag already at the head of `file` without
/// moving a single audio byte (#590). `Ok(false)` when it does not fit
/// and the caller must fall back to the rewrite.
///
/// Worth the trouble twice over. It is the difference between touching a
/// few kilobytes and moving the whole file — on a large file over a
/// network share, seconds against milliseconds — and it is also the safe
/// path: lofty's rewrite reads the entire file into memory, calls
/// `truncate(0)` on the original, and writes it back, so an interruption
/// anywhere in there leaves a truncated audio file. This writes a prefix
/// of exactly the length it replaces and never shortens anything.
///
/// The length check before the write is not a belt-and-braces assertion.
/// A padded encode that came back a different length than the arithmetic
/// promised would, written here, overlay the first frames of audio — so
/// a surprise is a reason to take the slow path, never to continue.
pub fn try_id3v2_in_place(
    file: &mut std::fs::File,
    tag: &lofty::id3::v2::Id3v2Tag,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use lofty::config::WriteOptions;
    use lofty::prelude::TagExt;
    use std::io::{Seek, SeekFrom, Write};

    let existing = read_id3v2_span(file)?;

    let mut bare = Vec::new();
    tag.dump_to(&mut bare, WriteOptions::new().preferred_padding(0))?;
    // lofty encodes a frameless tag as nothing at all, which is its way
    // of saying "strip this tag". Writing zero bytes over the old span
    // would leave the old tag in place and claim success, so removal
    // goes the long way round.
    if bare.is_empty() {
        return Ok(false);
    }
    let bare_len = u32::try_from(bare.len())?;

    let Some(padding) = fit_id3v2_padding(existing, bare_len) else {
        return Ok(false);
    };
    let target = existing.expect("a padding fit implies a span").total;

    let bytes = if padding == 0 {
        bare
    } else {
        let mut padded = Vec::new();
        tag.dump_to(&mut padded, WriteOptions::new().preferred_padding(padding))?;
        padded
    };
    if bytes.len() as u64 != u64::from(target) {
        return Ok(false);
    }

    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
