//! The ID3v2 tag a DSF file carries after its audio data.

use std::io;

/// Where a DSF file keeps its tag (#592).
///
/// DSF is the one container here whose metadata sits **after** the
/// audio, at an offset the header declares. That makes writing it the
/// easy case rather than the hard one: the tag can grow or shrink freely
/// and not one audio byte moves, because there is nothing behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsfLayout {
    /// The size the header claims the file is. Updated with the tag,
    /// because a player that trusts it and finds something else has
    /// every reason to call the file damaged.
    pub total_size: u64,
    /// Where the ID3v2 tag starts, or `0` for a file that has none.
    pub metadata_offset: u64,
    /// Where the `data` chunk ends, read from the chunk chain. A tag can
    /// only ever be written at or after this — see [`read_dsf_layout`].
    pub audio_end: u64,
}

/// The fixed part of a DSF header: magic, chunk size, file size, and the
/// metadata pointer.
const DSF_HEADER_LEN: u64 = 28;

/// Read the DSD chunk at the head of a DSF file.
///
/// `Ok(None)` when this is not a DSF at all, or when the header's own
/// numbers do not agree with the file — a metadata pointer past the end,
/// or one pointing into the header itself. Those are not files to write
/// a tag into on the strength of the pointer.
pub fn read_dsf_layout(file: &mut std::fs::File) -> io::Result<Option<DsfLayout>> {
    use std::io::{Read, Seek, SeekFrom};

    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; DSF_HEADER_LEN as usize];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &header[0..4] != b"DSD " {
        return Ok(None);
    }
    let read_u64 = |at: usize| {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&header[at..at + 8]);
        u64::from_le_bytes(buf)
    };
    if read_u64(4) != DSF_HEADER_LEN {
        return Ok(None);
    }
    let metadata_offset = read_u64(20);
    // Where the audio ends, read from the chunk chain rather than
    // trusted from the pointer. A header claiming its tag starts inside
    // the data chunk is not merely odd: writing there would overwrite
    // audio and then truncate the file to the end of the tag, which
    // destroys the track. Nothing but the chunk chain can tell us that
    // the pointer is wrong.
    let audio_end = read_dsf_audio_end(file, len)?;
    let Some(audio_end) = audio_end else {
        return Ok(None);
    };
    if metadata_offset != 0 && (metadata_offset < audio_end || metadata_offset > len) {
        return Ok(None);
    }
    Ok(Some(DsfLayout {
        total_size: read_u64(12),
        metadata_offset,
        audio_end,
    }))
}

/// Walk a DSF's chunk chain and report where the `data` chunk ends.
///
/// `Ok(None)` for a chain this cannot follow — a truncated file, a chunk
/// whose declared size runs past the end, or one with no `data` chunk at
/// all. Each of those is a file whose audio boundary we do not know, and
/// not knowing it is the whole reason to refuse.
fn read_dsf_audio_end(file: &mut std::fs::File, len: u64) -> io::Result<Option<u64>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut at = DSF_HEADER_LEN;
    loop {
        if at >= len {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 12];
        match file.read_exact(&mut header) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err),
        }
        let mut size_bytes = [0u8; 8];
        size_bytes.copy_from_slice(&header[4..12]);
        let size = u64::from_le_bytes(size_bytes);
        // A chunk smaller than its own header, or one running past the
        // end of the file, means the chain is not one to follow.
        if size < 12 {
            return Ok(None);
        }
        let end = match at.checked_add(size) {
            Some(end) if end <= len => end,
            _ => return Ok(None),
        };
        if &header[0..4] == b"data" {
            return Ok(Some(end));
        }
        at = end;
    }
}

/// Replace a DSF file's ID3v2 tag, and tell the header about it (#592).
///
/// An empty `tag` removes it: the file is cut back to the end of the
/// audio and the pointer set to zero, which is how a DSF says it has no
/// metadata.
///
/// Ordered so that an interruption always leaves the header declaring
/// **less** than the file physically holds, never more. The bytes go
/// down first, then the header fields, and the truncation last: a
/// reader trusts the declared size, so trailing bytes past it are
/// ignored, while a `total_size` larger than the file is what makes a
/// player call the file damaged.
///
/// This is not atomic — nothing in place can be — but every window it
/// leaves is one a reader survives.
pub fn write_dsf_id3v2(file: &mut std::fs::File, tag: &[u8]) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    let Some(layout) = read_dsf_layout(file)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a DSF file, or its header does not describe itself",
        ));
    };
    // With no tag on record, the tag goes right after the audio — read
    // from the chunk chain, not taken from the file's length, so trailing
    // junk is replaced rather than preserved ahead of the tag.
    let offset = if layout.metadata_offset != 0 {
        layout.metadata_offset
    } else {
        layout.audio_end
    };

    file.seek(SeekFrom::Start(offset))?;
    file.write_all(tag)?;
    let new_len = offset + tag.len() as u64;

    file.seek(SeekFrom::Start(12))?;
    file.write_all(&new_len.to_le_bytes())?;
    file.seek(SeekFrom::Start(20))?;
    let pointer = if tag.is_empty() { 0 } else { offset };
    file.write_all(&pointer.to_le_bytes())?;

    // Last, so the header never describes a file longer than it is.
    // Only ever cuts the tag region: `offset` is at or after the end of
    // the audio by construction.
    file.set_len(new_len)?;
    Ok(())
}

#[cfg(test)]
mod tests;
