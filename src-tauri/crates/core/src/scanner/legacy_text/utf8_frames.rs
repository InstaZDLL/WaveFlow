//! An ID3v2 frame that declares UTF-8 and is not.
//!
//! Where a frame that declares Latin-1 comes out as mojibake, one that
//! declares UTF-8 (`3`) over bytes that are not UTF-8 makes lofty refuse the
//! whole file ("failed to decode UTF-8 sequence"): the scanner then lists it
//! by its file name, with no tag at all. ID3v2.3 has no UTF-8 in the first
//! place, so a tool that writes `3` there is not writing UTF-8 either.
//!
//! The recovery changes nothing on disk. It walks the tag's frames, finds the
//! text frames whose declared UTF-8 does not hold, and reads the file through
//! [`Patched`], which shows the encoding byte of those frames as `0`
//! (Latin-1). That is one byte for one byte, so every offset stays where it
//! was, and the Latin-1 reading goes through the repair of the parent module
//! like any other: a GBK title comes out as Chinese and a Latin-1 one as
//! itself.

use std::io::{self, Read, Seek, SeekFrom};

/// Largest ID3v2 tag walked: a tag this size is not a text-frame problem.
const MAX_TAG: u64 = 16 * 1024 * 1024;

/// Offsets of the encoding bytes to show as `0`: frames declaring UTF-8 over
/// bytes that are not. Empty when the file has nothing of the kind, or a tag
/// this walk does not follow (unsynchronisation, an extended header, frames
/// that are compressed, encrypted or unsynchronised).
pub fn invalid_utf8_frames<R: Read + Seek>(reader: &mut R) -> io::Result<Vec<u64>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 10];
    if reader.read_exact(&mut header).is_err() || &header[..3] != b"ID3" {
        return Ok(Vec::new());
    }
    let version = header[3];
    let tag_flags = header[5];
    // 0x80 unsynchronisation, 0x40 extended header: bytes move, offsets lie.
    if !matches!(version, 3 | 4) || tag_flags & 0xc0 != 0 {
        return Ok(Vec::new());
    }
    let Some(size) = syncsafe(&header[6..10]).filter(|size| u64::from(*size) <= MAX_TAG) else {
        return Ok(Vec::new());
    };
    let mut body = vec![0u8; size as usize];
    if reader.read_exact(&mut body).is_err() {
        return Ok(Vec::new());
    }

    let mut offsets = Vec::new();
    let mut at = 0usize;
    while at + 10 <= body.len() {
        let id = &body[at..at + 4];
        // Padding, or the start of the audio.
        if id[0] == 0
            || !id
                .iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            break;
        }
        let raw = &body[at + 4..at + 8];
        let length = if version == 4 {
            syncsafe(raw).map(|n| n as usize)
        } else {
            Some(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize)
        };
        let Some(length) = length else { break };
        let flags = [body[at + 8], body[at + 9]];
        let start = at + 10;
        let Some(end) = start.checked_add(length).filter(|end| *end <= body.len()) else {
            break;
        };
        // v2.4: 0x0c compression/encryption, 0x02 unsynchronisation, 0x01 a
        // data-length indicator. v2.3: 0xc0 compression/encryption, 0x20
        // grouping. In all of them the byte after the header is not the
        // encoding.
        let plain = if version == 4 {
            flags[1] & 0x0f == 0
        } else {
            flags[1] & 0xe0 == 0
        };
        if plain && length >= 2 && is_text_frame(id) && body[start] == 3 {
            let text = &body[start + 1..end];
            if std::str::from_utf8(text).is_err() {
                offsets.push(10 + start as u64);
            }
        }
        at = end;
    }
    Ok(offsets)
}

/// `T…` text frames other than the user-defined `TXXX` (description first),
/// whose bytes are all text, and the frames with a language field: those
/// would need the walk to know where the text starts, so they stay out.
fn is_text_frame(id: &[u8]) -> bool {
    id[0] == b'T' && id != b"TXXX"
}

fn syncsafe(bytes: &[u8]) -> Option<u32> {
    if bytes.iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    Some(bytes.iter().fold(0, |n, b| (n << 7) | u32::from(*b)))
}

/// A reader that shows `0` at given offsets of the one it wraps.
pub struct Patched<R> {
    inner: R,
    offsets: Vec<u64>,
    position: u64,
}

impl<R: Read + Seek> Patched<R> {
    pub fn new(mut inner: R, offsets: Vec<u64>) -> io::Result<Self> {
        let position = inner.stream_position()?;
        Ok(Self {
            inner,
            offsets,
            position,
        })
    }
}

impl<R: Read + Seek> Read for Patched<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        for offset in &self.offsets {
            if let Some(i) = offset
                .checked_sub(self.position)
                .filter(|i| (*i as usize) < n)
            {
                buf[i as usize] = 0;
            }
        }
        self.position += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for Patched<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.position = self.inner.seek(to)?;
        Ok(self.position)
    }
}
