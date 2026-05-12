//! Metadata extraction from DSF / DFF containers.
//!
//! - **DSF** stores an ID3v2 blob at the offset advertised in the
//!   first chunk's `metadata_offset` field. We delegate parsing to
//!   the `id3` crate, which accepts a raw `Read` source.
//!
//! - **DFF** uses native IFF chunks: `DIIN` (DSDIFF Information,
//!   carrying child chunks `DITI` for title and `DIAR` for artist),
//!   `COMT` (comments), `ID3 ` (an optional embedded ID3v2 blob in
//!   newer files). We only handle the common subset — title /
//!   artist / album / year — to mirror what the rest of the scanner
//!   surfaces.
//!
//! Returns a [`DsdMetadata`] that the caller folds into the existing
//! `ExtractedFile` shape used by the scanner.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use super::parser::DsdContainer;

/// Free-text fields lifted from the container. All optional — DSD
/// rips often have nothing tagged at all.
#[derive(Debug, Default, Clone)]
pub struct DsdMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub genre: Option<String>,
}

impl DsdMetadata {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.year.is_none()
            && self.track_number.is_none()
            && self.disc_number.is_none()
            && self.genre.is_none()
    }
}

/// Read metadata from an open DSF file. The DSF header carries the
/// metadata offset at byte 20 (a u64 little-endian); 0 means "no
/// metadata".
pub fn read_dsf_metadata(file: &mut File) -> std::io::Result<DsdMetadata> {
    file.seek(SeekFrom::Start(20))?;
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf)?;
    let offset = u64::from_le_bytes(buf);
    if offset == 0 {
        return Ok(DsdMetadata::default());
    }
    file.seek(SeekFrom::Start(offset))?;
    Ok(parse_id3v2_blob(file))
}

/// Walk the DFF container looking for DIIN / ID3 chunks. The chunk
/// layout matches the parser module's traversal — we re-read it
/// here to keep the metadata path independent of the bitstream
/// parser (different lifetimes and buffering needs).
pub fn read_dff_metadata(file: &mut File) -> std::io::Result<DsdMetadata> {
    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    if file.read(&mut magic)? != 4 || &magic != b"FRM8" {
        return Ok(DsdMetadata::default());
    }
    skip(file, 8)?; // form size
    let mut form_type = [0u8; 4];
    if file.read(&mut form_type)? != 4 || &form_type != b"DSD " {
        return Ok(DsdMetadata::default());
    }

    let mut meta = DsdMetadata::default();
    loop {
        let mut chunk_id = [0u8; 4];
        if file.read(&mut chunk_id)? != 4 {
            break;
        }
        let mut size_buf = [0u8; 8];
        if file.read(&mut size_buf)? != 8 {
            break;
        }
        let chunk_size = u64::from_be_bytes(size_buf);
        let chunk_start = file.stream_position()?;
        match &chunk_id {
            b"DIIN" => walk_diin(file, chunk_start + chunk_size, &mut meta)?,
            b"ID3 " => {
                let blob = parse_id3v2_blob(file);
                meta.merge(blob);
            }
            b"COMT"
                // COMT carries an array of comments; we lift the
                // first non-empty one as a fallback title when DIIN
                // didn't supply one.
                if meta.title.is_none() => {
                    if let Some(comment) = read_first_comt_text(file, chunk_size) {
                        meta.title = Some(comment);
                    }
                }
            _ => {}
        }
        let next = chunk_start + chunk_size;
        // IFF word-alignment: an odd byte count is followed by a
        // single padding byte.
        let aligned = if next & 1 == 1 { next + 1 } else { next };
        file.seek(SeekFrom::Start(aligned))?;
    }
    Ok(meta)
}

fn walk_diin(file: &mut File, end: u64, meta: &mut DsdMetadata) -> std::io::Result<()> {
    while file.stream_position()? < end {
        let mut sub_id = [0u8; 4];
        if file.read(&mut sub_id)? != 4 {
            break;
        }
        let mut size_buf = [0u8; 8];
        if file.read(&mut size_buf)? != 8 {
            break;
        }
        let sub_size = u64::from_be_bytes(size_buf);
        let sub_start = file.stream_position()?;
        match &sub_id {
            b"DITI" => meta.title = read_diin_text(file, sub_size),
            b"DIAR" => meta.artist = read_diin_text(file, sub_size),
            _ => {}
        }
        let next = sub_start + sub_size;
        let aligned = if next & 1 == 1 { next + 1 } else { next };
        file.seek(SeekFrom::Start(aligned))?;
    }
    Ok(())
}

/// DIIN text chunks are `<u16 BE length><utf8 bytes>`. Returns
/// `None` for empty / malformed entries so the caller's `is_some()`
/// gating just works.
fn read_diin_text(file: &mut File, sub_size: u64) -> Option<String> {
    if sub_size < 2 {
        return None;
    }
    let mut len_buf = [0u8; 2];
    file.read_exact(&mut len_buf).ok()?;
    let len = u16::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return None;
    }
    let mut text = vec![0u8; len];
    file.read_exact(&mut text).ok()?;
    let s = String::from_utf8(text).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// COMT layout is `<u16 numComments> [<comment header><u16 count>
/// <utf8 bytes>]…`. Each comment header is 8 bytes: timeStamp (2),
/// type (2), reference (2), count (2). We skip headers and lift the
/// first non-empty comment as a courtesy fallback.
fn read_first_comt_text(file: &mut File, chunk_size: u64) -> Option<String> {
    if chunk_size < 2 {
        return None;
    }
    let mut count_buf = [0u8; 2];
    file.read_exact(&mut count_buf).ok()?;
    let count = u16::from_be_bytes(count_buf);
    for _ in 0..count {
        let mut header = [0u8; 8];
        if file.read_exact(&mut header).is_err() {
            return None;
        }
        // Last 2 bytes of the header carry the comment text length.
        let text_len = u16::from_be_bytes([header[6], header[7]]) as usize;
        if text_len == 0 {
            continue;
        }
        let mut text = vec![0u8; text_len];
        if file.read_exact(&mut text).is_err() {
            return None;
        }
        if let Ok(s) = String::from_utf8(text) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn parse_id3v2_blob<R: Read + Seek>(reader: &mut R) -> DsdMetadata {
    use id3::TagLike;
    // The `id3` crate parses an ID3v2 tag from any Read source —
    // ideal for the DSF footer where lofty would refuse the stream.
    let tag = match id3::Tag::read_from2(reader) {
        Ok(t) => t,
        Err(err) => {
            tracing::debug!(?err, "DSD ID3v2 parse failed");
            return DsdMetadata::default();
        }
    };
    DsdMetadata {
        title: tag.title().map(|s| s.to_string()),
        artist: tag.artist().map(|s| s.to_string()),
        album: tag.album().map(|s| s.to_string()),
        year: tag.year().map(|y| y as i64),
        track_number: tag.track().map(|t| t as i64),
        disc_number: tag.disc().map(|d| d as i64),
        // `genre()` on TagLike returns the raw TCON string (numeric
        // ID3v1 codes like "(13)" or free-text). The scanner already
        // canonicalises genres downstream so we forward as-is.
        genre: tag.genre().map(|s| s.to_string()),
    }
}

impl DsdMetadata {
    /// Overlay non-empty fields from `other` on top of `self`. Used
    /// when a DFF file carries both DIIN and an embedded ID3 blob —
    /// ID3 wins because it's the richer format, but DIIN values
    /// survive when ID3 is missing the field.
    fn merge(&mut self, other: DsdMetadata) {
        if other.title.is_some() {
            self.title = other.title;
        }
        if other.artist.is_some() {
            self.artist = other.artist;
        }
        if other.album.is_some() {
            self.album = other.album;
        }
        if other.year.is_some() {
            self.year = other.year;
        }
        if other.track_number.is_some() {
            self.track_number = other.track_number;
        }
        if other.disc_number.is_some() {
            self.disc_number = other.disc_number;
        }
        if other.genre.is_some() {
            self.genre = other.genre;
        }
    }
}

/// Pick the right reader based on the container detected by the
/// parser. Wraps both formats behind one entry point so the scanner
/// doesn't need to branch on the container type.
pub fn read_metadata(file: &mut File, container: DsdContainer) -> std::io::Result<DsdMetadata> {
    match container {
        DsdContainer::Dsf => read_dsf_metadata(file),
        DsdContainer::Dff => read_dff_metadata(file),
    }
}

fn skip<R: Seek>(r: &mut R, n: i64) -> std::io::Result<()> {
    r.seek(SeekFrom::Current(n)).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tempfile(bytes: &[u8]) -> (tempfile::NamedTempFile, File) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.flush().unwrap();
        let file = File::open(tmp.path()).unwrap();
        (tmp, file)
    }

    #[test]
    fn dsf_with_zero_metadata_offset_returns_empty() {
        // Build a minimal DSF header with metadata_offset = 0.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(&28u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes()); // metadata offset = 0
        let (_tmp, mut file) = write_tempfile(&bytes);
        let meta = read_dsf_metadata(&mut file).unwrap();
        assert!(meta.is_empty());
    }

    #[test]
    fn dff_with_diin_extracts_title_and_artist() {
        // Build a minimal DFF: FRM8 → DSD → DIIN → DITI + DIAR.
        let mut diin_payload = Vec::new();
        // DITI sub-chunk
        diin_payload.extend_from_slice(b"DITI");
        let title = "My Song";
        let title_bytes = title.as_bytes();
        let diti_size = (2 + title_bytes.len()) as u64;
        diin_payload.extend_from_slice(&diti_size.to_be_bytes());
        diin_payload.extend_from_slice(&(title_bytes.len() as u16).to_be_bytes());
        diin_payload.extend_from_slice(title_bytes);
        // pad if odd
        if diti_size & 1 == 1 {
            diin_payload.push(0);
        }
        // DIAR sub-chunk
        diin_payload.extend_from_slice(b"DIAR");
        let artist = "Some Artist";
        let artist_bytes = artist.as_bytes();
        let diar_size = (2 + artist_bytes.len()) as u64;
        diin_payload.extend_from_slice(&diar_size.to_be_bytes());
        diin_payload.extend_from_slice(&(artist_bytes.len() as u16).to_be_bytes());
        diin_payload.extend_from_slice(artist_bytes);
        if diar_size & 1 == 1 {
            diin_payload.push(0);
        }

        let mut diin_chunk = Vec::new();
        diin_chunk.extend_from_slice(b"DIIN");
        diin_chunk.extend_from_slice(&(diin_payload.len() as u64).to_be_bytes());
        diin_chunk.extend_from_slice(&diin_payload);

        let mut form_payload = Vec::new();
        form_payload.extend_from_slice(b"DSD ");
        form_payload.extend_from_slice(&diin_chunk);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FRM8");
        bytes.extend_from_slice(&(form_payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&form_payload);

        let (_tmp, mut file) = write_tempfile(&bytes);
        let meta = read_dff_metadata(&mut file).unwrap();
        assert_eq!(meta.title.as_deref(), Some("My Song"));
        assert_eq!(meta.artist.as_deref(), Some("Some Artist"));
        assert!(meta.album.is_none());
    }

    #[test]
    fn dff_with_no_metadata_returns_empty() {
        // FRM8 + DSD form with no DIIN/COMT/ID3 chunks.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FRM8");
        bytes.extend_from_slice(&4u64.to_be_bytes());
        bytes.extend_from_slice(b"DSD ");
        let (_tmp, mut file) = write_tempfile(&bytes);
        let meta = read_dff_metadata(&mut file).unwrap();
        assert!(meta.is_empty());
    }

    #[test]
    fn merge_overlays_only_non_empty_fields() {
        let mut base = DsdMetadata {
            title: Some("Old".into()),
            artist: Some("Keep me".into()),
            ..Default::default()
        };
        let overlay = DsdMetadata {
            title: Some("New".into()),
            ..Default::default()
        };
        base.merge(overlay);
        assert_eq!(base.title.as_deref(), Some("New"));
        assert_eq!(base.artist.as_deref(), Some("Keep me"));
    }
}
