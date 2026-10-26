use super::*;

/// A minimal but valid AIFF, so lofty will open it and write to it.
fn tiny_aiff() -> Vec<u8> {
    let frames: u32 = 8;
    let channels: u16 = 2;
    let sample_size: u16 = 16;
    let data_len = frames as usize * channels as usize * 2;

    let mut comm = Vec::new();
    comm.extend_from_slice(&channels.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&sample_size.to_be_bytes());
    comm.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);

    let mut ssnd = Vec::new();
    ssnd.extend_from_slice(&0u32.to_be_bytes());
    ssnd.extend_from_slice(&0u32.to_be_bytes());
    ssnd.resize(8 + data_len, 0);

    let mut body = Vec::new();
    body.extend_from_slice(b"AIFF");
    body.extend_from_slice(b"COMM");
    body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    body.extend_from_slice(&comm);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&((8 + data_len) as u32).to_be_bytes());
    body.extend_from_slice(&ssnd);

    let mut out = Vec::new();
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// AIFF was the one container the tag editor could write and this
/// module could not read. It fell through to the catch-all arm and
/// answered `Some(vec![])` -- "opened it, nothing here" -- which is
/// the answer that tells the scanner to clear the track's stored
/// rows. So a custom tag added through the editor went into the
/// file and left the library on the next scan, every scan.
///
/// Written with lofty and read back, rather than asserting on a
/// match arm: the arm existing proves nothing about whether it
/// finds what the writer put there.
#[test]
fn a_custom_tag_written_into_an_aiff_is_read_back() {
    use lofty::config::WriteOptions;
    use lofty::id3::v2::{ExtendedTextFrame, Frame, Id3v2Tag};
    use lofty::tag::{Accessor, TagExt};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("track.aiff");
    std::fs::write(&path, tiny_aiff()).expect("write aiff");

    // The concrete tag, not the generic one: `Tag` has no way to
    // carry a `TXXX` frame -- which is the whole reason this module
    // exists, and why writing through it would test nothing.
    let mut tag = Id3v2Tag::default();
    tag.set_title("Ghost".to_string());
    tag.insert(Frame::UserText(ExtendedTextFrame::new(
        lofty::TextEncoding::UTF8,
        "MY_CUSTOM".to_string(),
        "kept".to_string(),
    )));
    tag.save_to_path(&path, WriteOptions::default())
        .expect("save id3v2 into the aiff");

    // The file really is an AIFF lofty recognises, not something
    // that happens to parse: a wrong answer here would make the
    // assertion below pass for the wrong reason.
    let probed = Probe::open(&path)
        .expect("probe")
        .guess_file_type()
        .expect("guess");
    assert_eq!(probed.file_type(), Some(FileType::Aiff));

    let tags = read_extra_tags(&path).expect("aiff opened");
    assert!(
        tags.iter().any(|(k, v)| k == "MY_CUSTOM" && v == "kept"),
        "custom tag lost: {tags:?}"
    );
    // The title is modelled, so it is not offered as a custom
    // column -- the same rule every other container follows here.
    assert!(
        !tags.iter().any(|(k, _)| k.eq_ignore_ascii_case("TITLE")),
        "a modelled field leaked into the custom set: {tags:?}"
    );
}

#[test]
fn a_value_is_cut_on_a_character_boundary() {
    let mut out = Vec::new();
    // Multi-byte characters straddling the cap: slicing on the byte
    // index alone would panic here.
    let long = "é".repeat(MAX_VALUE);
    keep(&mut out, "NOTE", &long);
    assert_eq!(out.len(), 1);
    assert!(out[0].1.len() <= MAX_VALUE + 1);
    assert!(out[0].1.chars().all(|c| c == 'é'));
}

#[test]
fn the_first_value_of_a_repeated_key_wins() {
    let mut out = Vec::new();
    keep(&mut out, "SOURCE", "CD");
    keep(&mut out, "SOURCE", "Vinyl");
    assert_eq!(out, vec![("SOURCE".to_string(), "CD".to_string())]);
}

/// The same field reaching `keep` under two spellings is one
/// column, not two. An MP3 carrying `TCOM` and a `TXXX:Composer`
/// is the case that motivated this: the generic half names the
/// first `COMPOSER`, the remainder hands over the description
/// verbatim, and a case-sensitive test would offer both.
#[test]
fn a_key_repeated_in_another_case_is_the_same_key() {
    let mut out = Vec::new();
    keep(&mut out, "COMPOSER", "Ravel");
    keep(&mut out, "Composer", "Maurice Ravel");
    keep(&mut out, "composer", "M. Ravel");
    assert_eq!(out, vec![("COMPOSER".to_string(), "Ravel".to_string())]);
}

/// The stored spelling is the canonical one, not the one the
/// tagger happened to write. It is the column's identity -- the
/// picker lists it, the layout names it, the cell lookup binds it --
/// so it cannot depend on which file was read first.
#[test]
fn a_key_is_stored_in_its_canonical_case() {
    let mut out = Vec::new();
    keep(&mut out, "Ripper", "EAC");
    assert_eq!(out, vec![("RIPPER".to_string(), "EAC".to_string())]);
}

#[test]
fn empty_and_oversized_keys_are_dropped() {
    let mut out = Vec::new();
    keep(&mut out, "  ", "value");
    keep(&mut out, "KEY", "   ");
    keep(&mut out, &"K".repeat(MAX_KEY + 1), "value");
    assert!(out.is_empty());
}

/// The values WaveFlow already models must not come back as a
/// second, stale copy in a column of their own.
#[test]
fn keys_the_library_already_owns_are_skipped() {
    let mut out = Vec::new();
    for key in [
        "REPLAYGAIN_TRACK_GAIN",
        "replaygain_album_peak",
        "SYNCEDLYRICS",
        "MusicBrainz Track Id",
        "Lyrics",
    ] {
        keep(&mut out, key, "x");
    }
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn an_ordinary_custom_key_is_kept() {
    let mut out = Vec::new();
    keep(&mut out, "CATALOGNUMBER", "SRCL-1234");
    keep(&mut out, "RIPPER", "EAC 1.6");
    assert_eq!(out.len(), 2);
}

/// An unreadable or unsupported file is a track with no custom
/// tags, never an error: this feeds an optional column set.
#[test]
fn an_unreadable_file_is_none_not_empty() {
    // `None` and `Some(vec![])` drive different writes: one leaves
    // the stored tags alone, the other replaces them with nothing.
    assert!(read_extra_tags(Path::new("no-such-file.flac")).is_none());
}
