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

/// The two fields the tag editor writes into a DSF and the scan has
/// to read back, or the next scan undoes the edit.
#[test]
fn dsf_reads_the_album_artist_and_the_composer() {
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    tag.set_title("Bolero");
    tag.set_album_artist("  Orchestre National  ");
    tag.set_text("TCOM", "Maurice Ravel");
    let mut blob = Vec::new();
    tag.write_to(&mut blob, id3::Version::Id3v24).unwrap();

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DSD ");
    bytes.extend_from_slice(&28u64.to_le_bytes());
    bytes.extend_from_slice(&((28 + blob.len()) as u64).to_le_bytes());
    bytes.extend_from_slice(&28u64.to_le_bytes()); // metadata right after the header
    bytes.extend_from_slice(&blob);
    let (_tmp, mut file) = write_tempfile(&bytes);
    let meta = read_dsf_metadata(&mut file).unwrap();
    assert_eq!(meta.album_artist.as_deref(), Some("Orchestre National"));
    assert_eq!(meta.composer.as_deref(), Some("Maurice Ravel"));
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
