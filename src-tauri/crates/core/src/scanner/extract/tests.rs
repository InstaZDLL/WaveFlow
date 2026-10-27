use super::*;
use crate::scanner::canonical_name;

/// An AIFF saved as `.mp3` — the shape of an AAC a download site
/// renamed (#873) — is read as what it contains. By its extension
/// alone lofty refused it, and the scanner dropped a playable file.
#[test]
fn a_file_is_read_by_its_content_not_its_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("renamed.mp3");
    write_bytes(&path, &tiny_aiff(256));

    assert!(
        lofty::read_from_path(&path).is_err(),
        "the extension alone misleads lofty, which is the bug"
    );
    use lofty::file::TaggedFileExt;
    let file = read_tagged_file(&path).expect("read by content");
    // And a file nothing can read reports why, not only that it failed.
    let noise = dir.path().join("noise.mp3");
    write_bytes(&noise, &[0x5a; 4096]);
    match read_tagged_file(&noise) {
        Ok(_) => panic!("noise is not audio"),
        Err(err) => assert!(err.contains(": "), "the cause is kept: {err}"),
    }
    assert_eq!(file.file_type(), FileType::Aiff);
}

/// The last resort judges by what playback can do: a stream the
/// player decodes is described, anything else is refused.
#[test]
fn a_playable_stream_is_described_and_noise_is_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tone = dir.path().join("tone.mp3");
    write_bytes(&tone, &tiny_aiff(44_100));
    let info = probe_playable_stream(&tone).expect("the player can play it");
    assert_eq!(info.sample_rate, Some(44_100));
    assert!((900..=1100).contains(&info.duration_ms), "{info:?}");
    assert_eq!(
        info.codec.as_deref(),
        Some("PCM"),
        "the stream's codec, not the `.mp3` the file is called"
    );

    let noise = dir.path().join("noise.mp3");
    write_bytes(&noise, &[0x5a; 4096]);
    assert!(probe_playable_stream(&noise).is_none());
}

fn write_bytes(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("write fixture");
}

/// The scan that follows a tag write overwrites `track.rating` with
/// what it reads back, so the two scales have to meet exactly. They
/// did not: the write truncates a byte to 0-100 and the read
/// truncated it back, which sent every half star down by one.
#[test]
fn every_star_the_widget_can_set_survives_the_round_trip() {
    use lofty::prelude::*;
    use lofty::tag::{Tag, TagType};

    // What `Math.round(stars / 5 * 255)` produces, halves included.
    for byte in [26u8, 51, 77, 102, 128, 153, 179, 204, 230, 255] {
        // The conversion `TagPatch::Rating` writes for these
        // containers.
        let as_100 = ((byte as u16) * 100 / 255) as u8;
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::Popularimeter, as_100.to_string());
        assert_eq!(
            extract_rating(&tag),
            Some(byte),
            "{byte} went out as {as_100} and came back as something else"
        );
    }
}

/// lofty 0.25 hands a POPM frame to the generic tag as
/// `<provider>|<stars>|<counter>` rather than as the raw frame body
/// it used to be, and its own documentation still describes the old
/// shape. Reading only the old one is why an MP3's rating came back
/// as nothing after that bump.
#[test]
fn a_rating_is_read_from_both_shapes_lofty_has_used() {
    use lofty::prelude::*;
    use lofty::tag::{Tag, TagType};

    let mut id3 = Tag::new(TagType::Id3v2);
    id3.insert_text(ItemKey::Popularimeter, "MusicBee|4|0".to_string());
    assert_eq!(extract_rating(&id3), Some(204), "four stars of five");

    // The provider name is optional, and a rating can be anonymous.
    let mut anonymous = Tag::new(TagType::Id3v2);
    anonymous.insert_text(ItemKey::Popularimeter, "|5|17".to_string());
    assert_eq!(extract_rating(&anonymous), Some(255));

    // A plain number is a Vorbis / MP4 `RATING`, on the 0-100 scale,
    // and must not be read as a star count.
    let mut vorbis = Tag::new(TagType::VorbisComments);
    vorbis.insert_text(ItemKey::Popularimeter, "76".to_string());
    assert_eq!(extract_rating(&vorbis), Some(194));

    // Nothing that is neither — including a string that happens to
    // carry a pipe and a digit without being a popularimeter.
    for value in ["not a rating", "MusicBee|4", "x|4|0|extra", "a|4|later"] {
        let mut nonsense = Tag::new(TagType::Id3v2);
        nonsense.insert_text(ItemKey::Popularimeter, value.to_string());
        assert_eq!(extract_rating(&nonsense), None, "{value}");
    }
}

/// Smallest valid 1x1 JPEG — enough to satisfy the non-empty check
/// and exercise the hash + write + spawn_thumbnail_job pipeline
/// without dragging the `image` crate into the unit test.
const TINY_JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0xFF, 0xD9,
];

/// Wrap one packet in an Ogg beginning-of-stream page.
///
/// Enough of a page for the codec magic that follows the header to be
/// found where a reader looks for it, which is all identifying the
/// stream needs. The CRC is left zero: nothing on this path verifies
/// it, and a real encoder is a heavy price for one header.
fn ogg_page(packet: &[u8]) -> Vec<u8> {
    let mut page = Vec::new();
    page.extend_from_slice(b"OggS");
    page.push(0); // stream structure version
    page.push(0x02); // beginning of stream
    page.extend_from_slice(&0u64.to_le_bytes()); // granule position
    page.extend_from_slice(&1u32.to_le_bytes()); // bitstream serial
    page.extend_from_slice(&0u32.to_le_bytes()); // page sequence
    page.extend_from_slice(&0u32.to_le_bytes()); // checksum
    page.push(1); // one segment
    page.push(packet.len() as u8);
    page.extend_from_slice(packet);
    page
}

fn opus_identification() -> Vec<u8> {
    let mut packet = b"OpusHead".to_vec();
    packet.push(1); // version
    packet.push(2); // channels
    packet.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
    packet.extend_from_slice(&48_000u32.to_le_bytes()); // input rate
    packet.extend_from_slice(&0i16.to_le_bytes()); // output gain
    packet.push(0); // channel mapping family
    packet
}

fn speex_identification() -> Vec<u8> {
    let mut packet = b"Speex   ".to_vec();
    packet.extend_from_slice(&[0u8; 20]); // version string
    packet.extend_from_slice(&1u32.to_le_bytes()); // version id
    packet.extend_from_slice(&80u32.to_le_bytes()); // header size
    packet.extend_from_slice(&16_000u32.to_le_bytes()); // rate
    packet.extend_from_slice(&[0u8; 40]); // the rest, unread here
    packet
}

fn vorbis_identification() -> Vec<u8> {
    let mut packet = vec![1];
    packet.extend_from_slice(b"vorbis");
    packet.extend_from_slice(&0u32.to_le_bytes()); // version
    packet.push(2); // channels
    packet.extend_from_slice(&44_100u32.to_le_bytes()); // sample rate
    packet.extend_from_slice(&[0u8; 12]); // the three bitrate hints
    packet.push(0xB8); // block sizes
    packet.push(1); // framing
    packet
}

/// The bug this guards: `.ogg` names a container, not a codec, so a
/// stream we cannot decode reached a list built for Vorbis, was
/// indexed, and then failed the moment it was played.
///
/// Opus used to be that stream and is now the counter-example — the
/// decoder landed in #581 — so the case is kept as proof the check
/// judges by what we can actually play rather than by a fixed list
/// of codecs. Speex takes over as the refused one.
#[test]
fn an_ogg_is_judged_by_its_stream_and_not_by_its_extension() {
    let dir = tempfile::tempdir().expect("tempdir");

    let opus = dir.path().join("opus-in-disguise.ogg");
    write_bytes(&opus, &ogg_page(&opus_identification()));
    assert!(
        is_scannable_audio(&opus),
        "an Opus stream is playable now, whatever the extension says"
    );

    let speex = dir.path().join("speex-in-disguise.ogg");
    write_bytes(&speex, &ogg_page(&speex_identification()));
    assert!(
        !is_scannable_audio(&speex),
        "a stream with no decoder must not be indexed"
    );

    let vorbis = dir.path().join("really-vorbis.ogg");
    write_bytes(&vorbis, &ogg_page(&vorbis_identification()));
    assert!(
        is_scannable_audio(&vorbis),
        "the container the extension was built for still passes"
    );

    // An unambiguous extension is not opened at all, so a file the
    // extractor will refuse later still reaches it — an unreadable
    // file has to be reported, not made to disappear.
    let broken = dir.path().join("truncated.flac");
    write_bytes(&broken, b"not really audio");
    assert!(is_scannable_audio(&broken));

    // And an ambiguous container nothing can identify is left to the
    // extractor for the same reason.
    let unreadable = dir.path().join("empty.oga");
    write_bytes(&unreadable, b"");
    assert!(is_scannable_audio(&unreadable));

    let cover = dir.path().join("cover.jpg");
    write_bytes(&cover, TINY_JPEG);
    assert!(!is_scannable_audio(&cover));
}

/// A minimal but real AIFF: `FORM`/`COMM`/`SSND`, 16-bit big-endian
/// stereo PCM. Built here rather than committed as a fixture so the
/// bytes the test relies on are visible next to the assertions.
fn tiny_aiff(frames: u32) -> Vec<u8> {
    let channels: u16 = 2;
    let sample_size: u16 = 16;
    let data_len = frames as usize * channels as usize * 2;

    let mut comm = Vec::new();
    comm.extend_from_slice(&channels.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&sample_size.to_be_bytes());
    // 44100 Hz as an IEEE 754 80-bit extended float: biased exponent
    // 16383 + 15, then 0xAC44 (44100) left-aligned in the significand,
    // whose leading integer bit is explicit in this format.
    comm.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);

    let mut ssnd = Vec::new();
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // offset
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // block size
    for frame in 0..frames {
        // A quiet ramp rather than silence: a decoder that returns the
        // right frame count of zeroes would pass on silence alone.
        let sample = (frame as i16).wrapping_mul(64);
        ssnd.extend_from_slice(&sample.to_be_bytes());
        ssnd.extend_from_slice(&sample.to_be_bytes());
    }

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

/// The `aiff` feature is what makes the extension list below honest:
/// the scanner only offers what the engine can play, so this decodes a
/// real file rather than asserting that a string is in a list.
#[test]
fn an_aiff_file_probes_and_decodes() {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tone.aiff");
    write_bytes(&path, &tiny_aiff(256));

    let file = fs::File::open(&path).expect("open");
    let mut hint = Hint::new();
    hint.with_extension("aiff");
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("symphonia probes AIFF once the feature is on");

    let track = format
        .default_track(TrackType::Audio)
        .expect("the AIFF carries an audio track");
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .expect("audio codec params")
        .clone();
    let mut decoder = crate::audio_format::opus::codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .expect("AIFF holds the PCM `wav` already decodes");

    let mut decoded = 0u64;
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        let buffer = decoder.decode(&packet).expect("decode");
        decoded += buffer.frames() as u64;
    }
    assert_eq!(decoded, 256, "every frame written comes back out");
}

/// The list states a policy, and the policy has edges that are easy to
/// widen by accident.
#[test]
fn the_extension_list_admits_aiff_and_still_refuses_what_cannot_play() {
    for accepted in ["aiff", "aif", "wav", "flac", "mp3", "opus"] {
        assert!(
            AUDIO_EXTENSIONS.contains(&accepted),
            "{accepted} is playable and must be indexed"
        );
    }
    for refused in [
        // Proprietary, and staying out.
        "wma",
        // AIFC's compressed forms are refused by the reader, so
        // accepting the extension would index files that cannot
        // play.
        "aifc",
    ] {
        assert!(
            !AUDIO_EXTENSIONS.contains(&refused),
            "{refused} cannot be played and must not be indexed"
        );
    }
}

#[test]
fn folder_cover_picks_priority_stem_over_alphabetical_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let folder = dir.path().join("album");
    fs::create_dir_all(&folder).unwrap();

    // `albumart` ranks below `cover` in FOLDER_COVER_STEMS; even though
    // it sorts first alphabetically, the priority list must win.
    write_bytes(&folder.join("albumart.jpg"), TINY_JPEG);
    write_bytes(&folder.join("cover.png"), TINY_JPEG);

    let track = folder.join("01.flac");
    write_bytes(&track, b"not really audio");

    let cover = extract_folder_cover(&track, &artwork_dir).expect("cover found");
    assert_eq!(
        cover.format, "png",
        "cover.png should win over albumart.jpg"
    );
    assert_eq!(cover.source, ArtworkSource::Folder);
}

#[test]
fn folder_cover_normalises_jpeg_extension() {
    let dir = tempfile::tempdir().unwrap();
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let folder = dir.path().join("album");
    fs::create_dir_all(&folder).unwrap();

    write_bytes(&folder.join("front.JPEG"), TINY_JPEG);
    let track = folder.join("01.flac");
    write_bytes(&track, b"x");

    let cover = extract_folder_cover(&track, &artwork_dir).expect("cover found");
    // `jpeg` must collapse to `jpg` so the artwork dir has one
    // canonical extension per MIME.
    assert_eq!(cover.format, "jpg");
}

#[test]
fn folder_cover_returns_none_when_no_match() {
    let dir = tempfile::tempdir().unwrap();
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let folder = dir.path().join("album");
    fs::create_dir_all(&folder).unwrap();

    // Recognised extension but stem isn't in the priority list.
    write_bytes(&folder.join("scan-of-booklet.jpg"), TINY_JPEG);
    let track = folder.join("01.flac");
    write_bytes(&track, b"x");

    assert!(extract_folder_cover(&track, &artwork_dir).is_none());
}

#[test]
fn artist_image_finds_stem_in_parent_folder() {
    // Layout: <root>/<Artist>/<Album>/<track>
    let dir = tempfile::tempdir().unwrap();
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let artist_dir = dir.path().join("Daft Punk");
    let album_dir = artist_dir.join("Discovery");
    fs::create_dir_all(&album_dir).unwrap();

    write_bytes(&artist_dir.join("artist.jpg"), TINY_JPEG);
    let track = album_dir.join("01.flac");
    write_bytes(&track, b"x");

    let cover = extract_artist_image(&track, &canonical_name("Daft Punk"), &artwork_dir)
        .expect("artist image found two levels up");
    assert_eq!(cover.source, ArtworkSource::Folder);
    assert_eq!(cover.format, "jpg");
}

#[test]
fn artist_image_matches_canonical_name_stem() {
    // Layout: <root>/<Album>/<track> with `<Artist>.jpg` beside the album.
    let dir = tempfile::tempdir().unwrap();
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let album_dir = dir.path().join("Discovery");
    fs::create_dir_all(&album_dir).unwrap();

    write_bytes(&album_dir.join("Daft Punk.png"), TINY_JPEG);
    let track = album_dir.join("01.flac");
    write_bytes(&track, b"x");

    let cover = extract_artist_image(&track, &canonical_name("daft punk"), &artwork_dir)
        .expect("canonical-name stem match");
    assert_eq!(cover.format, "png");
}

#[test]
fn artist_image_ignores_unrelated_named_image() {
    let dir = tempfile::tempdir().unwrap();
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let album_dir = dir.path().join("Discovery");
    fs::create_dir_all(&album_dir).unwrap();

    // `cover.jpg` is an album cover, not an artist photo.
    write_bytes(&album_dir.join("cover.jpg"), TINY_JPEG);
    let track = album_dir.join("01.flac");
    write_bytes(&track, b"x");

    assert!(
        extract_artist_image(&track, &canonical_name("Daft Punk"), &artwork_dir).is_none(),
        "should not pick up album cover as artist image",
    );
}

#[test]
fn artist_image_returns_none_for_empty_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let folder = dir.path().join("album");
    fs::create_dir_all(&folder).unwrap();
    write_bytes(&folder.join("artist.jpg"), TINY_JPEG);
    let track = folder.join("01.flac");
    write_bytes(&track, b"x");

    // Empty canonical → defensive bail-out so we don't match every dir.
    assert!(extract_artist_image(&track, "", &artwork_dir).is_none());
}

#[test]
fn folder_cover_writes_hash_addressed_file() {
    let dir = tempfile::tempdir().unwrap();
    let artwork_dir = dir.path().join("artwork");
    fs::create_dir_all(&artwork_dir).unwrap();
    let folder = dir.path().join("album");
    fs::create_dir_all(&folder).unwrap();

    write_bytes(&folder.join("cover.jpg"), TINY_JPEG);
    let track = folder.join("01.flac");
    write_bytes(&track, b"x");

    let cover = extract_folder_cover(&track, &artwork_dir).expect("cover");
    let on_disk = artwork_dir.join(format!("{}.{}", cover.hash, cover.format));
    assert!(on_disk.exists(), "hash-addressed file must be written");
}

#[test]
fn hash_file_small_is_content_sensitive() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.bin");
    let b = dir.path().join("b.bin");
    write_bytes(&a, b"hello world");
    write_bytes(&b, b"hello world");
    assert_eq!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
    write_bytes(&b, b"hello WORLD");
    assert_ne!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
}

#[test]
fn hash_file_large_detects_head_and_size_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.bin");
    let mut data = vec![7u8; 3 * 1024 * 1024];
    write_bytes(&path, &data);
    let base = hash_file(&path).unwrap();

    // A change inside the head window flips the hash.
    data[10] = 42;
    write_bytes(&path, &data);
    assert_ne!(base, hash_file(&path).unwrap());

    // A change inside the tail window flips the hash too (guards the
    // seek-to-end + read_exact of the tail chunk).
    data[10] = 7; // restore head
    let last = data.len() - 10;
    data[last] = 42;
    write_bytes(&path, &data);
    assert_ne!(base, hash_file(&path).unwrap());

    // A size change flips the hash even with otherwise-identical
    // head + tail (the length is folded into the digest).
    data[last] = 7; // restore tail
    data.push(7); // grow by one byte
    write_bytes(&path, &data);
    assert_ne!(base, hash_file(&path).unwrap());
}

#[test]
fn hash_file_large_blind_to_middle() {
    // Documents the partial-hash tradeoff: a byte strictly between
    // the head and tail windows doesn't change the digest. Distinct
    // real tracks never hit this — their head and/or size differ.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.bin");
    let mut data = vec![7u8; 3 * 1024 * 1024];
    write_bytes(&path, &data);
    let base = hash_file(&path).unwrap();

    data[1_500_000] = 99; // > 1 MiB (head end), < 2 MiB (tail start)
    write_bytes(&path, &data);
    assert_eq!(base, hash_file(&path).unwrap());
}
