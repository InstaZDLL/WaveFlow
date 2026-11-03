use super::*;

const AUDIO: &[u8] = b"these bytes are the audio, and not one of them may go";

/// A tag as Monkey's Audio writes it: one item, then a footer, no header.
fn headerless_tag() -> Vec<u8> {
    let mut items = Vec::new();
    items.extend_from_slice(&5u32.to_le_bytes());
    items.extend_from_slice(&0u32.to_le_bytes());
    items.extend_from_slice(b"Title\0Hello");
    let mut tag = items.clone();
    tag.extend_from_slice(PREAMBLE);
    tag.extend_from_slice(&VERSION_2.to_le_bytes());
    tag.extend_from_slice(&((items.len() as u32) + FOOTER_LEN as u32).to_le_bytes());
    tag.extend_from_slice(&1u32.to_le_bytes());
    tag.extend_from_slice(&0u32.to_le_bytes());
    tag.extend_from_slice(&[0; 8]);
    tag
}

fn run(bytes: &[u8]) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("f");
    std::fs::write(&path, bytes).expect("write");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open");
    add_missing_ape_header(&mut file).expect("add");
    assert_eq!(file.stream_position().expect("position"), 0);
    drop(file);
    std::fs::read(&path).expect("read")
}

#[test]
fn a_headerless_tag_gets_the_header_and_the_audio_stays() {
    let bytes = [AUDIO, &headerless_tag()].concat();
    let out = run(&bytes);
    assert_eq!(out.len(), bytes.len() + 32);
    assert_eq!(&out[..AUDIO.len()], AUDIO);
    let header = &out[AUDIO.len()..AUDIO.len() + 32];
    assert_eq!(&header[..8], PREAMBLE);
    let flags = u32::from_le_bytes([header[20], header[21], header[22], header[23]]);
    assert_eq!(flags, (1 << 31) | (1 << 29));
    // The items follow, and the footer now admits to the header.
    assert_eq!(
        &out[AUDIO.len() + 32 + 8..AUDIO.len() + 32 + 19],
        b"Title\0Hello"
    );
    let footer = &out[out.len() - 32..];
    assert_eq!(&footer[..8], PREAMBLE);
    assert_eq!(footer[20..24], (1u32 << 31).to_le_bytes());
}

#[test]
fn what_sits_after_the_tag_stays_after_it() {
    let mut id3v1 = vec![0u8; 128];
    id3v1[..3].copy_from_slice(b"TAG");
    let bytes = [AUDIO, &headerless_tag(), &id3v1].concat();
    let out = run(&bytes);
    assert_eq!(out.len(), bytes.len() + 32);
    assert_eq!(&out[..AUDIO.len()], AUDIO);
    assert_eq!(&out[AUDIO.len()..AUDIO.len() + 8], PREAMBLE);
    assert_eq!(&out[out.len() - 128..], &id3v1[..]);
}

#[test]
fn every_other_file_is_left_exactly_as_it_was() {
    // No tag at all, and shorter than anything looked for.
    assert_eq!(run(AUDIO), AUDIO);
    assert_eq!(run(b"short"), b"short");
    assert_eq!(run(b""), b"");

    // A tag that already has its header.
    let mut flagged = headerless_tag();
    let at = flagged.len() - 12;
    flagged[at..at + 4].copy_from_slice(&(1u32 << 31).to_le_bytes());
    let bytes = [AUDIO, &flagged].concat();
    assert_eq!(run(&bytes), bytes);

    // APEv1, which has no header in lofty's arithmetic either.
    let mut v1 = headerless_tag();
    let at = v1.len() - 24;
    v1[at..at + 4].copy_from_slice(&1000u32.to_le_bytes());
    let bytes = [AUDIO, &v1].concat();
    assert_eq!(run(&bytes), bytes);

    // A size that does not fit the file is not a tag to touch.
    let mut lying = headerless_tag();
    let at = lying.len() - 20;
    lying[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let bytes = [AUDIO, &lying].concat();
    assert_eq!(run(&bytes), bytes);
}
