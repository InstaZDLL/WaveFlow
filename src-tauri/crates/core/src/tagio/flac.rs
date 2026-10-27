//! The FLAC comment block rewritten in place, inside the room the
//! metadata blocks and their padding already take.

use std::io;

/// FLAC metadata block types this module needs to tell apart. The rest
/// are carried through untouched, so they never need naming.
const FLAC_BLOCK_PADDING: u8 = 1;
const FLAC_BLOCK_VORBIS_COMMENT: u8 = 4;
/// A metadata region larger than this is not something to buffer in
/// memory on the strength of a length field we have not validated.
const FLAC_MAX_METADATA: u64 = 64 * 1024 * 1024;

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
    use lofty::ogg::OggPictureStorage;
    use std::io::{Seek, SeekFrom, Write};

    let Some(blocks) = read_flac_blocks(file)? else {
        return Ok(false);
    };
    // Everything the region holds today, headers included. This is the
    // budget the replacement has to land inside.
    let region: u64 = blocks
        .iter()
        .map(|block| block.content.len() as u64 + 4)
        .sum();

    // FLAC normally keeps its pictures in their own metadata blocks,
    // which is why they are copied through untouched above. But a
    // `VorbisComments` can carry pictures too — some taggers write the
    // base64 `METADATA_BLOCK_PICTURE` comment even in a FLAC, and lofty's
    // split/merge round trip can hand them back on the tag — and the
    // encoder here writes only the vendor string and the items. Encoding
    // a set that has pictures would drop them, which is the one thing
    // this path must never do. The rewrite knows how to write them.
    if !comments.pictures().is_empty() {
        return Ok(false);
    }

    let encoded = encode_vorbis_comments(comments);
    // A block's length field is 24 bits; a comment set larger than that
    // cannot be written as one block at all.
    if encoded.len() > 0x00ff_ffff {
        return Ok(false);
    }

    // The comment block is replaced and the padding is recomputed;
    // everything else stays exactly as it was.
    let kept: Vec<&FlacBlock> = blocks
        .iter()
        .filter(|block| block.ty != FLAC_BLOCK_VORBIS_COMMENT && block.ty != FLAC_BLOCK_PADDING)
        .collect();
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

    let mut out: Vec<u8> = Vec::with_capacity(region as usize);
    let write_block = |out: &mut Vec<u8>, ty: u8, content: &[u8], last: bool| {
        let len = content.len() as u32;
        out.push(if last { ty | 0x80 } else { ty });
        out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
        out.extend_from_slice(content);
    };
    let has_padding = padding.is_some();
    for block in &kept {
        write_block(&mut out, block.ty, &block.content, false);
    }
    write_block(&mut out, FLAC_BLOCK_VORBIS_COMMENT, &encoded, !has_padding);
    if let Some(len) = padding {
        write_block(&mut out, FLAC_BLOCK_PADDING, &vec![0u8; len as usize], true);
    }

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

#[cfg(test)]
mod tests;
