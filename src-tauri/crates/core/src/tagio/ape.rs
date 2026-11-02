//! An APEv2 tag with no header, made safe for lofty to replace.
//!
//! APEv2 makes the header optional, and Monkey's Audio leaves it out: its
//! default tag is items and a footer. lofty assumes every version 2 tag has
//! one and adds 32 to the size the footer states, so when it replaces such a
//! tag it removes 32 bytes too many, off the end of the audio. The file still
//! opens, its last frame fails its checksum, and the track ends early, on the
//! first edit and every later one.
//!
//! Giving the tag the header lofty expects makes its arithmetic right for
//! every container and every caller, and the result is a tag the format allows.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

const PREAMBLE: &[u8; 8] = b"APETAGEX";
const FOOTER_LEN: u64 = 32;
const ID3V1_LEN: u64 = 128;
const VERSION_2: u32 = 2000;
const CONTAINS_HEADER: u32 = 1 << 31;
const IS_HEADER: u32 = 1 << 29;
/// `LYRICS200`, then what precedes it: the Lyrics3v2 trailer.
const LYRICS3_MARKER: &[u8; 9] = b"LYRICS200";
const LYRICS3_SIZE_LEN: u64 = 6;

/// Where the part of the file that may end in an APE tag stops: before an
/// ID3v1 tag and a Lyrics3v2 block, which sit after it when they exist.
fn end_of_ape_region(file: &mut File, len: u64) -> io::Result<u64> {
    let mut end = len;
    if end >= ID3V1_LEN {
        let mut marker = [0u8; 3];
        file.seek(SeekFrom::Start(end - ID3V1_LEN))?;
        file.read_exact(&mut marker)?;
        if &marker == b"TAG" {
            end -= ID3V1_LEN;
        }
    }
    let trailer_len = LYRICS3_SIZE_LEN + LYRICS3_MARKER.len() as u64;
    if end >= trailer_len {
        let mut trailer = [0u8; 15];
        file.seek(SeekFrom::Start(end - trailer_len))?;
        file.read_exact(&mut trailer)?;
        if &trailer[6..] == LYRICS3_MARKER {
            let size = std::str::from_utf8(&trailer[..6])
                .ok()
                .and_then(|digits| digits.parse::<u64>().ok());
            if let Some(size) = size {
                end = end.saturating_sub(size + trailer_len);
            }
        }
    }
    Ok(end)
}

/// Insert the header a version 2 APE tag at the end of `file` lacks. Leaves
/// any other file exactly as it was, and the handle at the start of the file.
pub(super) fn add_missing_ape_header(file: &mut File) -> io::Result<()> {
    let len = file.metadata()?.len();
    let end = end_of_ape_region(file, len)?;
    if end >= FOOTER_LEN {
        let mut footer = [0u8; FOOTER_LEN as usize];
        file.seek(SeekFrom::Start(end - FOOTER_LEN))?;
        file.read_exact(&mut footer)?;
        let field = |at: usize| {
            u32::from_le_bytes([footer[at], footer[at + 1], footer[at + 2], footer[at + 3]])
        };
        let (version, size, flags) = (field(8), u64::from(field(12)), field(20));
        if &footer[..8] == PREAMBLE
            && version == VERSION_2
            && flags & CONTAINS_HEADER == 0
            && flags & IS_HEADER == 0
            && (FOOTER_LEN..=end).contains(&size)
        {
            let tag_at = end - size;
            let mut rest = Vec::with_capacity((len - tag_at) as usize);
            file.seek(SeekFrom::Start(tag_at))?;
            file.read_to_end(&mut rest)?;

            let mut header = footer;
            header[20..24].copy_from_slice(&(flags | CONTAINS_HEADER | IS_HEADER).to_le_bytes());
            // The footer says the same thing about the tag it closes.
            let footer_flags_at = (size - FOOTER_LEN) as usize + 20;
            rest[footer_flags_at..footer_flags_at + 4]
                .copy_from_slice(&(flags | CONTAINS_HEADER).to_le_bytes());

            file.seek(SeekFrom::Start(tag_at))?;
            file.write_all(&header)?;
            file.write_all(&rest)?;
        }
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

#[cfg(test)]
mod tests;
