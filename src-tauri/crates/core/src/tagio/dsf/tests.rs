use super::*;

/// The `fmt ` chunk a real DSF carries: a 12-byte header and a
/// 40-byte payload the guard never looks inside.
const FMT_CHUNK_LEN: u64 = 52;

/// Where the audio ends in a file built by [`dsf_file`].
fn audio_end_of(audio: &[u8]) -> u64 {
    DSF_HEADER_LEN + FMT_CHUNK_LEN + 12 + audio.len() as u64
}

/// A file shaped like a DSF, chunk chain included — the guard reads
/// it now, so a fixture without one would only ever prove refusals.
fn dsf_file(audio: &[u8], tag: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    dsf_file_with_pointer(audio, tag, None)
}

/// Same, with the metadata pointer forced to a value of our choosing
/// — for the headers that lie about where their tag is.
fn dsf_file_with_pointer(
    audio: &[u8],
    tag: &[u8],
    pointer: Option<u64>,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.dsf");
    let audio_end = audio_end_of(audio);
    let offset = match pointer {
        Some(forced) => forced,
        None if tag.is_empty() => 0,
        None => audio_end,
    };
    let total = audio_end + tag.len() as u64;

    let mut bytes = b"DSD ".to_vec();
    bytes.extend_from_slice(&DSF_HEADER_LEN.to_le_bytes());
    bytes.extend_from_slice(&total.to_le_bytes());
    bytes.extend_from_slice(&offset.to_le_bytes());

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&FMT_CHUNK_LEN.to_le_bytes());
    bytes.extend_from_slice(&[0u8; (FMT_CHUNK_LEN - 12) as usize]);

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(12 + audio.len() as u64).to_le_bytes());
    bytes.extend_from_slice(audio);

    bytes.extend_from_slice(tag);
    std::fs::write(&path, &bytes).expect("seed");
    (dir, path)
}

#[test]
fn a_pointer_into_the_audio_is_refused() {
    // The dangerous header. Writing at a pointer inside the data
    // chunk would overwrite audio and then cut the file back to the
    // end of the tag — the track destroyed by an edit to its title.
    // Only the chunk chain can say the pointer is wrong.
    let audio: Vec<u8> = (0..2048u32).map(|i| (i % 193) as u8 + 1).collect();
    let into_audio = DSF_HEADER_LEN + FMT_CHUNK_LEN + 12 + 512;
    let (_dir, path) = dsf_file_with_pointer(&audio, b"a tag", Some(into_audio));
    let before = std::fs::read(&path).expect("read");

    let mut file = open(&path);
    assert!(
        read_dsf_layout(&mut file).expect("layout").is_none(),
        "a pointer inside the data chunk is not a tag location"
    );
    assert!(write_dsf_id3v2(&mut file, b"replacement").is_err());
    drop(file);
    assert_eq!(
        std::fs::read(&path).expect("read"),
        before,
        "the refusal left the file exactly as it was"
    );
}

#[test]
fn a_file_with_no_data_chunk_is_refused() {
    // No data chunk means no audio boundary, and no boundary means
    // no safe place to put a tag.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chainless.dsf");
    let mut bytes = b"DSD ".to_vec();
    bytes.extend_from_slice(&DSF_HEADER_LEN.to_le_bytes());
    bytes.extend_from_slice(&80u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&FMT_CHUNK_LEN.to_le_bytes());
    bytes.extend_from_slice(&[0u8; (FMT_CHUNK_LEN - 12) as usize]);
    std::fs::write(&path, &bytes).expect("seed");

    let mut file = open(&path);
    assert!(read_dsf_layout(&mut file).expect("layout").is_none());
}

fn open(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open")
}

#[test]
fn a_bigger_tag_does_not_disturb_the_audio() {
    // The property that makes DSF the easy container: the tag is
    // last, so it can grow without anything moving.
    let audio: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8 + 1).collect();
    let (_dir, path) = dsf_file(&audio, b"small tag");

    let mut file = open(&path);
    let replacement = vec![0x42u8; 8192];
    write_dsf_id3v2(&mut file, &replacement).expect("write");
    drop(file);

    let after = std::fs::read(&path).expect("read");
    let audio_at = (audio_end_of(&audio) - audio.len() as u64) as usize;
    assert_eq!(
        &after[audio_at..audio_at + audio.len()],
        &audio[..],
        "the audio is byte for byte what it was"
    );
    assert_eq!(&after[audio_end_of(&audio) as usize..], &replacement[..]);

    let mut file = open(&path);
    let layout = read_dsf_layout(&mut file).expect("layout").expect("a DSF");
    assert_eq!(layout.metadata_offset, audio_end_of(&audio));
    assert_eq!(
        layout.total_size,
        after.len() as u64,
        "the header declares the size the file actually is"
    );
}

#[test]
fn a_smaller_tag_cuts_only_the_tag() {
    let audio: Vec<u8> = (0..1024u32).map(|i| (i % 97) as u8 + 1).collect();
    let (_dir, path) = dsf_file(&audio, &vec![0xEEu8; 4096]);

    let mut file = open(&path);
    write_dsf_id3v2(&mut file, b"tiny").expect("write");
    drop(file);

    let after = std::fs::read(&path).expect("read");
    assert_eq!(after.len() as u64, audio_end_of(&audio) + 4);
    let audio_at = (audio_end_of(&audio) - audio.len() as u64) as usize;
    assert_eq!(&after[audio_at..audio_at + audio.len()], &audio[..]);
}

#[test]
fn the_header_never_claims_more_than_the_file_holds() {
    // The ordering property. The write is not atomic — nothing in
    // place can be — so what matters is which side of the truth an
    // interruption leaves it on. A reader trusts the declared size,
    // so trailing bytes past it are ignored; a total_size larger
    // than the file is what makes a player call it damaged.
    let audio: Vec<u8> = vec![9; 1024];
    let (_dir, path) = dsf_file(&audio, &vec![0xCCu8; 4096]);

    let mut file = open(&path);
    write_dsf_id3v2(&mut file, b"much shorter").expect("write");
    drop(file);

    let physical = std::fs::metadata(&path).expect("meta").len();
    let mut file = open(&path);
    let layout = read_dsf_layout(&mut file).expect("layout").expect("a DSF");
    assert!(
        layout.total_size <= physical,
        "declared {} against a file of {physical}",
        layout.total_size
    );
    assert_eq!(layout.total_size, physical, "and here they agree exactly");
}

#[test]
fn a_file_that_had_no_tag_gets_one_at_the_end() {
    let audio: Vec<u8> = (0..512u32).map(|i| (i % 71) as u8 + 1).collect();
    let (_dir, path) = dsf_file(&audio, b"");

    let mut file = open(&path);
    assert_eq!(
        read_dsf_layout(&mut file)
            .expect("layout")
            .expect("a DSF")
            .metadata_offset,
        0
    );
    write_dsf_id3v2(&mut file, b"a brand new tag").expect("write");
    drop(file);

    let mut file = open(&path);
    let layout = read_dsf_layout(&mut file).expect("layout").expect("a DSF");
    assert_eq!(layout.metadata_offset, audio_end_of(&audio));
    let after = std::fs::read(&path).expect("read");
    assert_eq!(
        &after[layout.metadata_offset as usize..],
        b"a brand new tag"
    );
}

#[test]
fn an_empty_tag_removes_it_and_says_so() {
    let audio: Vec<u8> = vec![7; 256];
    let (_dir, path) = dsf_file(&audio, b"an existing tag");

    let mut file = open(&path);
    write_dsf_id3v2(&mut file, b"").expect("write");
    drop(file);

    let mut file = open(&path);
    let layout = read_dsf_layout(&mut file).expect("layout").expect("a DSF");
    assert_eq!(
        layout.metadata_offset, 0,
        "zero is how a DSF says it has none"
    );
    assert_eq!(layout.total_size, audio_end_of(&audio));
    assert_eq!(
        std::fs::read(&path).expect("read").len() as u64,
        audio_end_of(&audio)
    );
}

#[test]
fn a_header_that_does_not_describe_itself_is_refused() {
    // A pointer past the end of the file, or into the header: the
    // write would either land in the audio or past it, and neither
    // is a tag edit.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("bad.dsf");
    let mut bytes = b"DSD ".to_vec();
    bytes.extend_from_slice(&DSF_HEADER_LEN.to_le_bytes());
    bytes.extend_from_slice(&100u64.to_le_bytes());
    bytes.extend_from_slice(&9_000u64.to_le_bytes()); // past the end
    bytes.extend_from_slice(&[0u8; 64]);
    std::fs::write(&path, &bytes).expect("seed");

    let mut file = open(&path);
    assert!(read_dsf_layout(&mut file).expect("layout").is_none());
    assert!(write_dsf_id3v2(&mut file, b"tag").is_err());
}
