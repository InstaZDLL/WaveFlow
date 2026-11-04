//! The FLAC comment block rewritten in place, inside the room the
//! metadata blocks and their padding already take.

use std::io;

/// FLAC metadata block types this module needs to tell apart. The rest
/// are carried through untouched, so they never need naming.
const FLAC_BLOCK_PADDING: u8 = 1;
const FLAC_BLOCK_VORBIS_COMMENT: u8 = 4;
const FLAC_BLOCK_PICTURE: u8 = 6;
/// A metadata region larger than this is not something to buffer in
/// memory on the strength of a length field we have not validated.
const FLAC_MAX_METADATA: u64 = 64 * 1024 * 1024;
/// The room a rewrite leaves behind it, so the next edits of this file fit
/// in place instead of copying it again.
const FLAC_REWRITE_PADDING: u64 = 8192;
/// The picture types a cover edit replaces, as `TagPatch::Cover` defines
/// it: `Other` and `CoverFront`. Every other image survives one.
const FLAC_PICTURE_OTHER: u32 = 0;
const FLAC_PICTURE_FRONT: u32 = 3;

/// One metadata block as it sits on disk: its type, and its contents
/// byte for byte.
struct FlacBlock {
    ty: u8,
    content: Vec<u8>,
}

/// Serialise a Vorbis comment block's contents.
///
/// Little-endian lengths throughout, which is the one thing FLAC borrows
/// from Ogg rather than from its own big-endian headers — getting it
/// backwards produces a block every player rejects.
fn encode_vorbis_comments(comments: &lofty::ogg::tag::VorbisComments) -> Vec<u8> {
    let mut out = Vec::new();
    let vendor = comments.vendor().as_bytes();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    // The count has to be written before the items, and the items are
    // behind an iterator, so they are gathered first.
    let mut entries: Vec<Vec<u8>> = Vec::new();
    for (key, value) in comments.items() {
        let mut entry = Vec::with_capacity(key.len() + value.len() + 1);
        entry.extend_from_slice(key.as_bytes());
        entry.push(b'=');
        entry.extend_from_slice(value.as_bytes());
        entries.push(entry);
    }
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        out.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry);
    }
    out
}

/// Read the metadata blocks at the head of a FLAC file.
///
/// `Ok(None)` for anything this path should not touch — a file that does
/// not start with `fLaC` (an ID3v2-prefixed FLAC lands here), or a
/// metadata region whose declared size is not credible.
fn read_flac_blocks(file: &mut std::fs::File) -> io::Result<Option<Vec<FlacBlock>>> {
    use std::io::{Read, Seek, SeekFrom};

    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &magic != b"fLaC" {
        return Ok(None);
    }

    // A read that runs out of file is this function's own answer — the
    // container is not one we can splice — and not an error the edit
    // should fail on. Anything else is a real I/O failure and is
    // propagated.
    macro_rules! read_or_decline {
        ($buf:expr) => {
            match file.read_exact($buf) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => return Err(err),
            }
        };
    }

    let mut blocks = Vec::new();
    let mut total: u64 = 0;
    loop {
        let mut header = [0u8; 4];
        read_or_decline!(&mut header);
        let last = header[0] & 0x80 != 0;
        let ty = header[0] & 0x7f;
        let len = u32::from(header[1]) << 16 | u32::from(header[2]) << 8 | u32::from(header[3]);
        total += u64::from(len) + 4;
        if total > FLAC_MAX_METADATA {
            return Ok(None);
        }
        let mut content = vec![0u8; len as usize];
        read_or_decline!(&mut content);
        blocks.push(FlacBlock { ty, content });
        if last {
            break;
        }
    }
    Ok(Some(blocks))
}

/// Write `comments` into the metadata region a FLAC file already has,
/// absorbing the size difference into its PADDING block (#590).
/// `Ok(false)` when it does not fit and the caller must fall back.
///
/// This is lofty's own open TODO (`lofty-rs#445`): its writer builds the
/// new block set and then calls a `replace_range` that shifts **every
/// audio byte** whenever the new metadata differs in size from the old,
/// in 64 KB chunks over the whole file. Padding exists precisely so that
/// never has to happen, and every tagger leaves some.
///
/// Only the comment block is regenerated. Pictures, seek table, cue
/// sheet and application blocks are copied through byte for byte, which
/// is both faster and the only way to honour the rule that an edit to a
/// title never costs a cover or a MusicBrainz identifier. A cover edit
/// therefore does not come here — regenerating picture blocks is the
/// rewrite's job.
pub fn try_flac_in_place(
    file: &mut std::fs::File,
    comments: &lofty::ogg::tag::VorbisComments,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use std::io::{Seek, SeekFrom, Write};

    let Some(blocks) = read_flac_blocks(file)? else {
        return Ok(false);
    };
    let region = region_len(&blocks);
    let Some(encoded) = encodable_comments(comments) else {
        return Ok(false);
    };

    // The comment block is replaced and the padding is recomputed;
    // everything else stays exactly as it was.
    let kept = kept_blocks(&blocks);
    let fixed: u64 = kept
        .iter()
        .map(|block| block.content.len() as u64 + 4)
        .sum::<u64>()
        + encoded.len() as u64
        + 4;

    let padding = match region.checked_sub(fixed) {
        // An exact fit needs no padding block at all.
        Some(0) => None,
        // A padding block cannot be smaller than its own header.
        Some(slack) if slack >= 4 => Some(slack - 4),
        _ => return Ok(false),
    };
    if padding.is_some_and(|len| len > 0x00ff_ffff) {
        return Ok(false);
    }

    let out = encode_region(&kept, &encoded, padding);

    // Same reasoning as the ID3v2 path: a region of the wrong length
    // written here would run into the audio frames behind it, so a
    // surprise sends us to the slow path rather than to the disk.
    if out.len() as u64 != region {
        return Ok(false);
    }

    file.seek(SeekFrom::Start(4))?;
    file.write_all(&out)?;
    Ok(true)
}

/// Everything the region holds today, headers included: the budget an
/// in-place write has to land inside.
fn region_len(blocks: &[FlacBlock]) -> u64 {
    blocks
        .iter()
        .map(|block| block.content.len() as u64 + 4)
        .sum()
}

/// The blocks an edit of the comments carries over byte for byte.
fn kept_blocks(blocks: &[FlacBlock]) -> Vec<&FlacBlock> {
    blocks
        .iter()
        .filter(|block| block.ty != FLAC_BLOCK_VORBIS_COMMENT && block.ty != FLAC_BLOCK_PADDING)
        .collect()
}

/// The comment block's contents, or `None` when this module cannot be the
/// one to write them.
fn encodable_comments(comments: &lofty::ogg::tag::VorbisComments) -> Option<Vec<u8>> {
    use lofty::ogg::OggPictureStorage;

    // FLAC normally keeps its pictures in their own metadata blocks,
    // which is why they are copied through untouched. But a
    // `VorbisComments` can carry pictures too — some taggers write the
    // base64 `METADATA_BLOCK_PICTURE` comment even in a FLAC, and lofty's
    // split/merge round trip can hand them back on the tag — and the
    // encoder here writes only the vendor string and the items. Encoding
    // a set that has pictures would drop them, which is the one thing
    // this path must never do. lofty knows how to write them.
    if !comments.pictures().is_empty() {
        return None;
    }
    let encoded = encode_vorbis_comments(comments);
    // A block's length field is 24 bits; a comment set larger than that
    // cannot be written as one block at all.
    (encoded.len() <= 0x00ff_ffff).then_some(encoded)
}

/// The metadata region: the kept blocks, the comments, and the padding
/// last, whose flag closes the region when there is one.
fn encode_region(kept: &[&FlacBlock], comments: &[u8], padding: Option<u64>) -> Vec<u8> {
    let write_block = |out: &mut Vec<u8>, ty: u8, content: &[u8], last: bool| {
        let len = content.len() as u32;
        out.push(if last { ty | 0x80 } else { ty });
        out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
        out.extend_from_slice(content);
    };
    let mut out = Vec::new();
    let has_padding = padding.is_some();
    for block in kept {
        write_block(&mut out, block.ty, &block.content, false);
    }
    write_block(&mut out, FLAC_BLOCK_VORBIS_COMMENT, comments, !has_padding);
    if let Some(len) = padding {
        write_block(&mut out, FLAC_BLOCK_PADDING, &vec![0u8; len as usize], true);
    }
    out
}

/// Grow the metadata region of the **copy** a rewrite works on, with room
/// left behind for the next edits. `Ok(false)` when lofty has to do it.
///
/// Only the comments change, which keeps the promise of the in-place path:
/// the other blocks go through byte for byte, among them a picture lofty
/// cannot parse, which it would discard on its way through.
fn grow_comments_on_copy(
    file: &mut std::fs::File,
    comments: &lofty::ogg::tag::VorbisComments,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use lofty::io::FileLike;

    let Some(blocks) = read_flac_blocks(file)? else {
        return Ok(false);
    };
    let Some(encoded) = encodable_comments(comments) else {
        return Ok(false);
    };
    let region = region_len(&blocks);
    let out = encode_region(&kept_blocks(&blocks), &encoded, Some(FLAC_REWRITE_PADDING));
    // Growing is what this is for. A region it would shrink is one with too
    // much padding for a single block, and lofty's `splice` truncates a
    // stream with no frames behind the region to the wrong length.
    if out.len() as u64 <= region {
        return Ok(false);
    }
    file.splice(4..4 + region, &out)?;
    Ok(true)
}

/// Write `flac`'s tags onto the copy `file` holds, with lofty.
///
/// `FlacFile::save_to` is the one writer in lofty that does not rewind before
/// it probes the format: on a handle it has just read a file through, it
/// reads no bytes at the end of the file and answers "failed to write to
/// file". And it refuses outright to write a non-empty ID3v2 tag, which FLAC
/// cannot officially hold; taken off what it saves, the tag stays in the file,
/// because the writer works behind it.
fn save_with_lofty(
    flac: &mut lofty::flac::FlacFile,
    file: &mut std::fs::File,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::config::WriteOptions;
    use lofty::file::AudioFile;
    use std::io::Seek;

    flac.remove_id3v2();
    file.rewind()?;
    flac.save_to(file, WriteOptions::default())
        .map_err(|err| super::with_causes(&err))?;
    Ok(())
}

/// Read back what a rewrite just wrote: a comment lost here would otherwise
/// reach the user's file, and the rename is what makes that permanent.
fn check_comments_landed(
    flac: &lofty::flac::FlacFile,
    file: &mut std::fs::File,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;
    use std::io::Seek;

    // lofty's writer leaves out a comment with no value.
    fn items(flac: &lofty::flac::FlacFile) -> Vec<(String, String)> {
        flac.vorbis_comments()
            .into_iter()
            .flat_map(|comments| comments.items())
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    file.rewind()?;
    let options = ParseOptions::new()
        .read_properties(false)
        .read_cover_art(false);
    let written = lofty::flac::FlacFile::read_from(file, options)?;
    if items(&written) != items(flac) {
        return Err("the rewritten FLAC does not hold the comments it was given".into());
    }
    Ok(())
}

/// Put an edit of `flac`'s comments on the copy `file` holds, for a rewrite
/// through [`super::rewrite_via_temp`].
pub fn save_flac_comments(
    flac: &mut lofty::flac::FlacFile,
    file: &mut std::fs::File,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let grown = match flac.vorbis_comments() {
        Some(comments) => grow_comments_on_copy(file, comments)?,
        None => false,
    };
    if !grown {
        save_with_lofty(flac, file)?;
    }
    check_comments_landed(flac, file)
}

/// Put an edit of `flac`'s pictures on the copy `file` holds, for a rewrite
/// through [`super::rewrite_via_temp`]. lofty regenerates the picture blocks.
pub fn save_flac_pictures(
    flac: &mut lofty::flac::FlacFile,
    file: &mut std::fs::File,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    save_with_lofty(flac, file)?;
    check_comments_landed(flac, file)
}

/// Where a FLAC file's parts sit, without buffering any of them.
struct FlacLayout {
    /// Type, offset of the contents, and their length, for each block.
    blocks: Vec<(u8, u64, u64)>,
    /// Where the frames begin, and they run to the end of the file.
    frames_at: u64,
}

/// The layout of a FLAC file, behind the ID3v2 tag some carry. `None` for a
/// file that does not read as one to its end.
fn read_layout(file: &mut std::fs::File) -> io::Result<Option<FlacLayout>> {
    use std::io::{Read, Seek, SeekFrom};

    const ID3V2_HEADER_LEN: u64 = 10;
    const ID3V2_HAS_FOOTER: u8 = 0x10;

    let len = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;

    macro_rules! read_or_none {
        ($buf:expr) => {
            match file.read_exact($buf) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => return Err(err),
            }
        };
    }

    let mut head = [0u8; ID3V2_HEADER_LEN as usize];
    read_or_none!(&mut head);
    let mut start = 0;
    if &head[..3] == b"ID3" {
        let size = head[6..10]
            .iter()
            .fold(0u64, |size, byte| size << 7 | u64::from(byte & 0x7f));
        start = ID3V2_HEADER_LEN + size;
        if head[5] & ID3V2_HAS_FOOTER != 0 {
            start += ID3V2_HEADER_LEN;
        }
    }
    file.seek(SeekFrom::Start(start))?;
    let mut magic = [0u8; 4];
    read_or_none!(&mut magic);
    if &magic != b"fLaC" {
        return Ok(None);
    }

    let mut blocks = Vec::new();
    let mut at = start + 4;
    loop {
        let mut header = [0u8; 4];
        file.seek(SeekFrom::Start(at))?;
        read_or_none!(&mut header);
        let block_len =
            u64::from(header[1]) << 16 | u64::from(header[2]) << 8 | u64::from(header[3]);
        if at + 4 + block_len > len {
            return Ok(None);
        }
        blocks.push((header[0] & 0x7f, at + 4, block_len));
        at += 4 + block_len;
        if header[0] & 0x80 != 0 {
            return Ok(Some(FlacLayout {
                blocks,
                frames_at: at,
            }));
        }
    }
}

/// The contents of the blocks `wanted` picks, in the order of the file.
fn read_contents(
    file: &mut std::fs::File,
    layout: &FlacLayout,
    wanted: impl Fn(u8) -> bool,
) -> io::Result<Vec<Vec<u8>>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut contents = Vec::new();
    for &(_, at, len) in layout.blocks.iter().filter(|(ty, ..)| wanted(*ty)) {
        let mut content = vec![0u8; len as usize];
        file.seek(SeekFrom::Start(at))?;
        file.read_exact(&mut content)?;
        contents.push(content);
    }
    Ok(contents)
}

/// A picture block's type and image, by the block's own lengths. `None` for
/// one that does not read, which lofty would have nothing to rewrite from.
fn picture_parts(block: &[u8]) -> Option<(u32, &[u8])> {
    fn take_u32(rest: &mut &[u8]) -> Option<u32> {
        let (value, tail) = rest.split_first_chunk::<4>()?;
        *rest = tail;
        Some(u32::from_be_bytes(*value))
    }
    let mut rest = block;
    let kind = take_u32(&mut rest)?;
    let mime = take_u32(&mut rest)? as usize;
    rest = rest.get(mime..)?;
    let description = take_u32(&mut rest)? as usize;
    rest = rest.get(description..)?;
    // Width, height, colour depth and colour count.
    rest = rest.get(16..)?;
    let image = take_u32(&mut rest)? as usize;
    Some((kind, rest.get(..image)?))
}

/// Whether two handles read the same bytes from `a_at` and `b_at` to the end
/// of their files.
fn same_tail(
    a: &mut std::fs::File,
    a_at: u64,
    b: &mut std::fs::File,
    b_at: u64,
) -> io::Result<bool> {
    use std::io::{BufReader, Read, Seek, SeekFrom};

    if a.seek(SeekFrom::End(0))? - a_at != b.seek(SeekFrom::End(0))? - b_at {
        return Ok(false);
    }
    a.seek(SeekFrom::Start(a_at))?;
    b.seek(SeekFrom::Start(b_at))?;
    let mut a = BufReader::with_capacity(64 * 1024, a);
    let mut b = BufReader::with_capacity(64 * 1024, b);
    let mut a_chunk = [0u8; 16 * 1024];
    let mut b_chunk = [0u8; 16 * 1024];
    loop {
        let read = a.read(&mut a_chunk)?;
        if read == 0 {
            return Ok(true);
        }
        b.read_exact(&mut b_chunk[..read])?;
        if a_chunk[..read] != b_chunk[..read] {
            return Ok(false);
        }
    }
}

/// Refuse a rewrite of a FLAC that took anything an edit of its tags may not
/// touch: this runs on every one, before the rename can make it permanent.
///
/// The frames and the STREAMINFO, byte for byte, and the seek table, cue
/// sheet and application blocks, in their order. Of the pictures, every image
/// but the ones a cover edit replaces, and at least one where a cover was.
///
/// Silent on a file that does not read as FLAC to begin with: a rewrite that
/// lands here for another container has nothing to answer for.
pub(super) fn check_rewrite(
    original: &mut std::fs::File,
    rewritten: &mut std::fs::File,
) -> io::Result<()> {
    let refuse = |what: &str| {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the rewrite would have {what}, so the original was left as it was"),
        ))
    };

    let Some(before) = read_layout(original)? else {
        return Ok(());
    };
    let Some(after) = read_layout(rewritten)? else {
        return refuse("left a file that is not FLAC");
    };

    let structural = |ty: u8| {
        ty != FLAC_BLOCK_PADDING && ty != FLAC_BLOCK_VORBIS_COMMENT && ty != FLAC_BLOCK_PICTURE
    };
    if read_contents(original, &before, structural)?
        != read_contents(rewritten, &after, structural)?
    {
        return refuse("changed the stream's own metadata");
    }

    let is_picture = |ty: u8| ty == FLAC_BLOCK_PICTURE;
    let pictures_after = read_contents(rewritten, &after, is_picture)?;
    let images_after: Vec<&[u8]> = pictures_after
        .iter()
        .filter_map(|block| picture_parts(block).map(|(_, image)| image))
        .collect();
    let mut had_a_cover = false;
    for block in read_contents(original, &before, is_picture)? {
        match picture_parts(&block) {
            Some((FLAC_PICTURE_OTHER | FLAC_PICTURE_FRONT, _)) => had_a_cover = true,
            Some((_, image)) if images_after.contains(&image) => {}
            // One that does not parse survives by being copied whole.
            None if pictures_after.contains(&block) => {}
            _ => return refuse("lost an embedded image"),
        }
    }
    let has_a_cover = pictures_after.iter().any(|block| {
        matches!(
            picture_parts(block),
            Some((FLAC_PICTURE_OTHER | FLAC_PICTURE_FRONT, _))
        )
    });
    if had_a_cover && !has_a_cover {
        return refuse("lost the front cover");
    }

    if !same_tail(original, before.frames_at, rewritten, after.frames_at)? {
        return refuse("changed the audio");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
