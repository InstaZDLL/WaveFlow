use std::fs;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::prelude::*;
use lofty::tag::{MergeTag, SplitTag, Tag, TagType};
use symphonia::core::audio::sample::i24;
use symphonia::core::audio::{AudioBytes, GenericAudioBufferRef};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, Timestamp};

use super::codec_label;
use crate::scanner::extract::{is_scannable_audio, probe_playable_stream};

#[test]
fn codecs_are_spelled_as_the_format_chip_spells_them() {
    assert_eq!(codec_label("mp3"), "MP3");
    assert_eq!(codec_label("flac"), "FLAC");
    assert_eq!(codec_label("pcm_s16le"), "PCM");
    assert_eq!(codec_label("vorbis"), "Vorbis");
    assert_eq!(codec_label("opus"), "Opus");
    assert_eq!(codec_label("wavpack"), "WavPack");
    assert_eq!(codec_label("ape"), "APE");
}

/// Files made by the reference encoders (`wavpack` 5.9.0, `mac` 13.45), with
/// what the reference decoders output for each: its length and BLAKE3. Nothing
/// here comes from the code under test. The same source appears in both
/// formats on purpose, and its hash is the same in both.
const REFERENCE: &[(&str, usize, &str)] = &[
    (
        "s16_stereo.wv",
        6000,
        "472dfac69171dc97635b21a63a6c6514a1ad1fc9441d1dc804aef543e5a65f4f",
    ),
    (
        "s16_stereo_hh.wv",
        6000,
        "472dfac69171dc97635b21a63a6c6514a1ad1fc9441d1dc804aef543e5a65f4f",
    ),
    (
        "s24_stereo_x.wv",
        9000,
        "fbcf6ac8fa0dd05daa7314b7e7fbcfb679a2dbf68134564eaf9e9f84a7043766",
    ),
    (
        "s32_mono.wv",
        6000,
        "6881ca7df25f00a0b89efd772f8a5d915d2dcb918c82157e5c8bed1ac353df1b",
    ),
    (
        "s32_stereo.wv",
        12000,
        "a0a358ba3408af5dcec2b69bbd20b85d9ec2f177f8edafcf83aa727cb514992a",
    ),
    (
        "f32_stereo.wv",
        12000,
        "7f9995b9f1e1540fdc7771f98f5c00e36dfcc116bdd392407d8927240f5474a7",
    ),
    (
        "u8_stereo.wv",
        3000,
        "bba77e8a7ce91591b89eb5e6339a563eaf4b794aced313ce73c5baed12a11d82",
    ),
    (
        "s16_mono_f.wv",
        3000,
        "9f9b18fcff782c2a2a2ef16eefd2f7b0d6bc794dfb3fa5d72f3ad4160c39c3b2",
    ),
    (
        "s16_51.wv",
        18000,
        "de2f3401eb42a6a9a838b9460bb8efadf8ed344bcc0f59ddfe949df36cfdfe8f",
    ),
    (
        "s24_71.wv",
        36000,
        "1329a81edcb357f7bb1d90a0f84e0cca6f5d3df25246c5942b5698926bf83ba6",
    ),
    (
        "seek_blocks.wv",
        80000,
        "3f7f3531ae4997e65e640a3e45d7992767c8bec10a5d71649f73dab44021fab3",
    ),
    (
        "s16_stereo_c2000.ape",
        6000,
        "472dfac69171dc97635b21a63a6c6514a1ad1fc9441d1dc804aef543e5a65f4f",
    ),
    (
        "s24_stereo_c3000.ape",
        9000,
        "fbcf6ac8fa0dd05daa7314b7e7fbcfb679a2dbf68134564eaf9e9f84a7043766",
    ),
    (
        "s32_mono_c1000.ape",
        6000,
        "6881ca7df25f00a0b89efd772f8a5d915d2dcb918c82157e5c8bed1ac353df1b",
    ),
    (
        "u8_stereo_c2000.ape",
        3000,
        "bba77e8a7ce91591b89eb5e6339a563eaf4b794aced313ce73c5baed12a11d82",
    ),
    (
        "s16_mono_c4000.ape",
        3000,
        "9f9b18fcff782c2a2a2ef16eefd2f7b0d6bc794dfb3fa5d72f3ad4160c39c3b2",
    ),
    (
        "s16_stereo_c5000.ape",
        6000,
        "472dfac69171dc97635b21a63a6c6514a1ad1fc9441d1dc804aef543e5a65f4f",
    ),
    (
        "s16_51_c2000.ape",
        18000,
        "de2f3401eb42a6a9a838b9460bb8efadf8ed344bcc0f59ddfe949df36cfdfe8f",
    ),
    (
        "seek_frames_c1000.ape",
        640000,
        "1d98ebabc75a4bf510f728dd6141e0f359cea9ffe36f0de7cec78be0b219636a",
    ),
];

/// Streams the crates do not decode, each for its own reason.
const REFUSED: &[&str] = &[
    "hybrid_lossy.wv",
    "dsd_compressed.wv",
    "s32_stereo_c2000.ape",
    "f32_stereo_c2000.ape",
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lossless")
        .join(name)
}

fn open(path: &Path) -> Result<(Box<dyn FormatReader>, Box<dyn AudioDecoder>), Error> {
    let file = fs::File::open(path).expect("open fixture");
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = super::probe().probe(
        &hint,
        MediaSourceStream::new(Box::new(file), Default::default()),
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    let params = format
        .default_track(TrackType::Audio)
        .and_then(|track| track.codec_params.as_ref())
        .and_then(|params| params.audio())
        .expect("audio params")
        .clone();
    let decoder =
        super::opus::codecs().make_audio_decoder(&params, &AudioDecoderOptions::default())?;
    Ok((format, decoder))
}

/// Append a decoded buffer the way the reference tools write PCM; 8-bit as
/// unsigned, whichever way the decoder holds it.
fn append(buf: &GenericAudioBufferRef<'_>, out: &mut Vec<u8>) {
    macro_rules! bytes {
        ($b:expr, $s:ty) => {{
            let mut raw = vec![0u8; $b.byte_len_as::<$s>()];
            $b.copy_bytes_interleaved_as::<$s, _>(&mut raw[..]);
            out.extend_from_slice(&raw);
        }};
    }
    match buf {
        GenericAudioBufferRef::U8(b) => bytes!(b, u8),
        GenericAudioBufferRef::S8(b) => bytes!(b, u8),
        GenericAudioBufferRef::S16(b) => bytes!(b, i16),
        GenericAudioBufferRef::S24(b) => bytes!(b, i24),
        GenericAudioBufferRef::S32(b) => bytes!(b, i32),
        GenericAudioBufferRef::F32(b) => bytes!(b, f32),
        _ => panic!("sample format these streams do not produce"),
    }
}

fn decode_all(path: &Path) -> Vec<u8> {
    let (mut format, mut decoder) = open(path).expect("open");
    let mut pcm = Vec::new();
    while let Some(packet) = format.next_packet().expect("next packet") {
        append(&decoder.decode(&packet).expect("decode"), &mut pcm);
    }
    pcm
}

#[test]
fn decoded_audio_is_what_the_reference_decoder_outputs() {
    for (name, len, hash) in REFERENCE {
        let pcm = decode_all(&fixture(name));
        assert_eq!(pcm.len(), *len, "{name}: length");
        assert_eq!(
            blake3::hash(&pcm).to_hex().as_str(),
            *hash,
            "{name}: decoded bytes differ from the reference decoder's"
        );
    }
}

/// A seek has to land where it says, not on the block or frame boundary
/// before it. The fixtures have 512-frame blocks and 73 728-frame frames.
#[test]
fn a_seek_lands_on_the_exact_frame() {
    for (name, frame_bytes) in [("seek_blocks.wv", 4usize), ("seek_frames_c1000.ape", 4)] {
        let path = fixture(name);
        let full = decode_all(&path);
        let total = full.len() / frame_bytes;
        let (mut format, mut decoder) = open(&path).expect("open");

        // Out of order, so a target inside the frame already decoded and one
        // far from it both occur. The last is the very end of the stream.
        let targets = [
            total / 2,
            total / 2 + 1,
            1,
            total - 1,
            75_000 % total,
            0,
            total * 3 / 4 + 3,
            74_000 % total,
            total,
        ];
        for target in targets {
            let seeked = format
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Timestamp {
                        ts: Timestamp::new(target as i64),
                        track_id: 0,
                    },
                )
                .unwrap_or_else(|err| panic!("{name}: seek to {target}: {err}"));
            assert_eq!(seeked.actual_ts.get(), target as i64, "{name}");
            decoder.reset();

            let want = (total - target).min(3000) * frame_bytes;
            let mut got = Vec::new();
            let mut first_pts = None;
            while got.len() < want {
                let Some(packet) = format.next_packet().expect("next packet") else {
                    break;
                };
                first_pts.get_or_insert(packet.pts.get());
                append(&decoder.decode(&packet).expect("decode"), &mut got);
            }
            let start = target * frame_bytes;
            assert_eq!(&got[..want], &full[start..start + want], "{name} @{target}");
            if want > 0 {
                assert_eq!(first_pts, Some(target as i64), "{name} @{target}: stamp");
            }
        }
    }
}

/// `ActiveStream::seek_ms` seeks by time, not by timestamp.
#[test]
fn a_seek_by_time_lands_where_the_time_says() {
    for name in ["seek_blocks.wv", "seek_frames_c1000.ape"] {
        let (mut format, _) = open(&fixture(name)).expect("open");
        let seeked = format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::try_from_secs_f64(0.25).expect("time"),
                    track_id: Some(0),
                },
            )
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(seeked.actual_ts.get(), 11_025, "{name}");
    }
}

#[test]
fn a_seek_past_the_end_is_refused_rather_than_guessed() {
    for name in ["s16_stereo.wv", "s16_stereo_c2000.ape"] {
        let (mut format, _) = open(&fixture(name)).expect("open");
        let result = format.seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: Timestamp::new(1_000_000),
                track_id: 0,
            },
        );
        assert!(result.is_err(), "{name}");
    }
}

/// What is refused has to be refused by all of them at once: the probe, the
/// scanner's walk and the scanner's fallback. A file one of them accepts is
/// the unplayable row the one registry exists to prevent.
#[test]
fn a_refused_stream_is_refused_everywhere() {
    for name in REFUSED {
        let path = fixture(name);
        assert!(
            matches!(open(&path).map(|_| ()), Err(Error::Unsupported(_))),
            "{name}: the probe must say unsupported, not fall through to another reader"
        );
        assert!(!is_scannable_audio(&path), "{name}: scanner walk");
        assert!(probe_playable_stream(&path).is_none(), "{name}: fallback");
    }
}

#[test]
fn a_playable_stream_is_playable_everywhere() {
    for (name, _, _) in REFERENCE {
        let path = fixture(name);
        assert!(is_scannable_audio(&path), "{name}: scanner walk");
        let stream = probe_playable_stream(&path).unwrap_or_else(|| panic!("{name}: fallback"));
        let expected = if name.ends_with(".wv") {
            "WavPack"
        } else {
            "APE"
        };
        assert_eq!(stream.codec.as_deref(), Some(expected), "{name}");
        assert!(stream.duration_ms > 0, "{name}");
    }
    let info = probe_playable_stream(&fixture("s24_stereo_c3000.ape")).expect("ape");
    assert_eq!(info.sample_rate, Some(48_000));
    assert_eq!(info.channels, Some(2));
    assert_eq!(info.bit_depth, Some(24));
    let info = probe_playable_stream(&fixture("s16_51.wv")).expect("wv");
    assert_eq!(info.channels, Some(6));
    assert_eq!(info.bit_depth, Some(16));
}

/// Every way a stream can be damaged ends in an error or a shorter track.
/// Never a panic, which on the decoder thread takes playback down, and never
/// a packet that decodes to the wrong audio: both formats carry a CRC.
#[test]
fn a_damaged_stream_never_panics_or_plays_wrong_audio() {
    for name in ["seek_blocks.wv", "seek_frames_c1000.ape", "s16_51.wv"] {
        let path = fixture(name);
        let original = fs::read(&path).expect("read");
        let dir = tempfile::tempdir().expect("tempdir");
        let copy = dir.path().join(name);

        // Truncated at points through the header, the seek table and the audio.
        for len in [3, 12, 31, 33, 60, 200, 1_000, original.len() / 2] {
            fs::write(&copy, &original[..len.min(original.len())]).expect("write");
            let _ = is_scannable_audio(&copy);
            let _ = probe_playable_stream(&copy);
            if let Ok((mut format, mut decoder)) = open(&copy) {
                while let Ok(Some(packet)) = format.next_packet() {
                    let _ = decoder.decode(&packet);
                }
            }
        }

        // Damage anywhere at all, headers included. A debug build checks for
        // overflow as well, so a panic the release build would hide shows here.
        for (i, at) in (0..original.len())
            .step_by(original.len() / 97 + 1)
            .enumerate()
        {
            let mut damaged = original.clone();
            damaged[at] ^= 0x55u8.rotate_left(i as u32);
            fs::write(&copy, &damaged).expect("write");
            let _ = is_scannable_audio(&copy);
            if let Ok((mut format, mut decoder)) = open(&copy) {
                while let Ok(Some(packet)) = format.next_packet() {
                    let _ = decoder.decode(&packet);
                }
            }
        }

        // A flipped byte well into the audio: whatever still decodes has to
        // appear in the clean decode at the position it claims.
        let full = decode_all(&path);
        let frame_bytes = full.len() as u64 / total_frames(&path);
        let mut flipped = original.clone();
        let at = flipped.len() * 2 / 3;
        flipped[at] ^= 0xff;
        fs::write(&copy, &flipped).expect("write");
        let Ok((mut format, mut decoder)) = open(&copy) else {
            continue;
        };
        let mut skipped = 0;
        while let Ok(Some(packet)) = format.next_packet() {
            let pts = packet.pts.get() as u64;
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let mut pcm = Vec::new();
                    append(&decoded, &mut pcm);
                    let start = (pts * frame_bytes) as usize;
                    assert_eq!(
                        &pcm[..],
                        &full[start..start + pcm.len()],
                        "{name}: a damaged packet decoded to the wrong audio"
                    );
                }
                Err(_) => skipped += 1,
            }
        }
        assert!(skipped > 0, "{name}: the damage must be noticed");
    }
}

fn total_frames(path: &Path) -> u64 {
    let (format, _) = open(path).expect("open");
    format
        .default_track(TrackType::Audio)
        .and_then(|track| track.num_frames)
        .expect("declared length")
}

/// lofty reports no error for a file that merely has the extension, so the
/// refusal is what keeps it from becoming a silent row.
#[test]
fn a_file_that_only_has_the_extension_is_not_indexed() {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["junk.wv", "junk.ape"] {
        let path = dir.path().join(name);
        fs::write(&path, vec![0x41u8; 4096]).expect("write");
        assert!(!is_scannable_audio(&path), "{name}");
    }

    // A real header with no audio behind it: an unfinished Monkey's Audio file.
    let mut unfinished = fs::read(fixture("s16_stereo_c2000.ape")).expect("read");
    let descriptor = u32::from_le_bytes(unfinished[8..12].try_into().expect("four bytes")) as usize;
    let total_frames_at = descriptor + 12;
    unfinished[total_frames_at..total_frames_at + 4].copy_from_slice(&0u32.to_le_bytes());
    let path = dir.path().join("unfinished.ape");
    fs::write(&path, &unfinished).expect("write");
    assert!(!is_scannable_audio(&path));
    assert!(open(&path).is_err());
}

/// A tag ahead of a refused stream does not hide what it is.
#[test]
fn a_tag_ahead_of_a_hybrid_file_does_not_hide_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let hybrid = [
        id3v2_tag_of_ten_bytes(),
        fs::read(fixture("hybrid_lossy.wv")).expect("read"),
    ]
    .concat();
    let path = dir.path().join("hybrid.wv");
    fs::write(&path, hybrid).expect("write");
    assert!(!is_scannable_audio(&path));

    let playable = [
        id3v2_tag_of_ten_bytes(),
        fs::read(fixture("s16_stereo.wv")).expect("read"),
    ]
    .concat();
    let path = dir.path().join("playable.wv");
    fs::write(&path, playable).expect("write");
    assert!(is_scannable_audio(&path));
}

/// Older Monkey's Audio files are outside the crate. Refused, and with the
/// marker honoured: the probe does not go on to read the audio as some other
/// format.
#[test]
fn an_old_monkeys_audio_file_is_not_read_as_something_else() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("old.ape");
    let mut bytes = fs::read(fixture("s16_stereo_c2000.ape")).expect("read");
    bytes[4..6].copy_from_slice(&3930u16.to_le_bytes());
    // Before format 3980 the compression level follows the version.
    bytes[6..8].copy_from_slice(&2000u16.to_le_bytes());
    fs::write(&path, &bytes).expect("write");
    assert!(open(&path).is_err());
    assert!(probe_playable_stream(&path).is_none());
}

/// Taggers do put an ID3v2 tag ahead of a Monkey's Audio file, though the
/// format has no place for one. The seek table counts from the marker.
#[test]
fn a_tag_ahead_of_the_stream_does_not_shift_the_audio() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tagged.ape");
    let original = fs::read(fixture("seek_frames_c1000.ape")).expect("read");
    let mut bytes = id3v2_tag_of_ten_bytes();
    bytes.extend_from_slice(&original);
    fs::write(&path, &bytes).expect("write");

    let clean = decode_all(&fixture("seek_frames_c1000.ape"));
    assert_eq!(decode_all(&path), clean);

    let (mut format, mut decoder) = open(&path).expect("open");
    format
        .seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: Timestamp::new(100_000),
                track_id: 0,
            },
        )
        .expect("seek");
    let packet = format.next_packet().expect("next").expect("a packet");
    let mut pcm = Vec::new();
    append(&decoder.decode(&packet).expect("decode"), &mut pcm);
    assert_eq!(&pcm[..], &clean[400_000..400_000 + pcm.len()]);
}

/// The smallest ID3v2 tag there is: a header, and ten bytes of padding.
fn id3v2_tag_of_ten_bytes() -> Vec<u8> {
    let mut tag = b"ID3\x04\x00\x00\x00\x00\x00\x0a".to_vec();
    tag.extend_from_slice(&[0; 10]);
    tag
}

/// Four bytes spelling a marker inside another file are not that format:
/// being sure of them would stop the probe there and lose the real one.
#[test]
fn a_stray_marker_does_not_capture_another_format() {
    use symphonia::core::formats::probe::{Score, Scoreable};
    use symphonia::core::io::ScopedStream;

    fn stream(bytes: &[u8]) -> MediaSourceStream<'static> {
        MediaSourceStream::new(
            Box::new(std::io::Cursor::new(bytes.to_vec())),
            Default::default(),
        )
    }

    for junk in [
        // An impossible version, then a plausible one with no descriptor.
        &b"MAC \xff\xff\x00\x00\x00\x00\x00\x00"[..],
        &b"MAC \x96\x0f\x00\x00\x01\x00\x00\x00"[..],
        // A version from before descriptors, with no compression level.
        &b"MAC \x5a\x0f\x07\x00\x00\x00\x00\x00"[..],
    ] {
        let mut mss = stream(junk);
        let scoped = ScopedStream::new(&mut mss, junk.len() as u64);
        let score = super::ape::ApeReader::score(scoped).expect("score");
        assert!(matches!(score, Score::Unsupported), "{junk:?}");
    }

    let junk = [b"wvpk".as_slice(), &[0xff; 28]].concat();
    let mut mss = stream(&junk);
    let scoped = ScopedStream::new(&mut mss, junk.len() as u64);
    let score = super::wavpack::WavPackReader::score(scoped).expect("score");
    assert!(matches!(score, Score::Unsupported));
}

/// `edit::patch_file`'s round trip for these two containers, at the level
/// this crate can reach: the concrete file, split, patched, merged, saved.
#[test]
fn a_tag_edit_keeps_unknown_fields_and_leaves_the_audio_alone() {
    let dir = tempfile::tempdir().expect("tempdir");

    fn seed(tag: &mut lofty::ape::ApeTag) {
        use lofty::ape::ApeItem;
        use lofty::tag::ItemValue;
        for (key, value) in [
            ("Title", "Before"),
            ("MY_OWN_FIELD", "keep me"),
            ("REPLAYGAIN_TRACK_GAIN", "-6.50 dB"),
            ("REPLAYGAIN_TRACK_PEAK", "0.912345"),
        ] {
            tag.insert(ApeItem::new(key.into(), ItemValue::Text(value.into())).expect("item"));
        }
    }
    macro_rules! round_trip {
        ($ty:ty, $name:expr) => {{
            let path = dir.path().join($name);
            fs::copy(fixture($name), &path).expect("copy");
            let hash_before = blake3::hash(&decode_all(&path));

            // First a tag as another tool would have left it.
            let mut handle = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("open");
            let mut file = <$ty>::read_from(&mut handle, ParseOptions::new()).expect("read");
            let mut tag = lofty::ape::ApeTag::new();
            seed(&mut tag);
            file.set_ape(tag);
            handle.seek(SeekFrom::Start(0)).expect("rewind");
            file.save_to(&mut handle, WriteOptions::default())
                .expect("save");

            // Then the edit.
            handle.seek(SeekFrom::Start(0)).expect("rewind");
            let mut file = <$ty>::read_from(&mut handle, ParseOptions::new()).expect("read");
            let (remainder, mut generic) = file.remove_ape().unwrap_or_default().split_tag();
            generic.set_title("After".into());
            file.set_ape(remainder.merge_tag(generic));
            handle.seek(SeekFrom::Start(0)).expect("rewind");
            file.save_to(&mut handle, WriteOptions::default())
                .expect("save");
            drop(handle);

            let tagged = crate::scanner::extract::read_tagged_file(&path).expect("read back");
            let tag: &Tag = tagged.primary_tag().expect("tag");
            assert_eq!(tag.tag_type(), TagType::Ape, "{}", $name);
            assert_eq!(tag.title().as_deref(), Some("After"), "{}", $name);

            let gain = crate::scanner::replay_gain::extract_replay_gain(tag);
            assert_eq!(gain.track_gain_db, Some(-6.5), "{}: gain", $name);
            assert!(gain.track_peak.is_some(), "{}: peak", $name);

            let extra = crate::scanner::extra_tags::read_extra_tags(&path).expect("extra tags");
            assert!(
                extra
                    .iter()
                    .any(|(k, v)| k == "MY_OWN_FIELD" && v == "keep me"),
                "{}: the field no reader models must survive, got {extra:?}",
                $name
            );

            assert_eq!(
                blake3::hash(&decode_all(&path)),
                hash_before,
                "{}: the audio must come through a tag write untouched",
                $name
            );
            assert!(is_scannable_audio(&path), "{}", $name);
        }};
    }
    round_trip!(lofty::wavpack::WavPackFile, "s16_stereo.wv");
    round_trip!(lofty::ape::ApeFile, "s16_stereo_c2000.ape");
}

/// The tag Monkey's Audio writes has no header, and lofty, replacing one,
/// took 32 bytes of audio with it: the last frame failed its CRC from then on.
#[test]
fn replacing_a_tag_with_no_header_costs_no_audio() {
    use lofty::ape::{ApeFile, ApeItem, ApeTag};
    use lofty::tag::ItemValue;

    let items: &[u8] = b"\x05\0\0\0\0\0\0\0Title\0Hello";
    let mut footer = b"APETAGEX".to_vec();
    footer.extend_from_slice(&2000u32.to_le_bytes());
    footer.extend_from_slice(&((items.len() + 32) as u32).to_le_bytes());
    footer.extend_from_slice(&1u32.to_le_bytes());
    footer.extend_from_slice(&(1u32 << 30).to_le_bytes());
    footer.extend_from_slice(&[0; 8]);

    let source = fixture("s16_stereo_c2000.ape");
    let audio = fs::read(&source).expect("read");
    let clean = decode_all(&source);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tagged.ape");
    fs::write(&path, [&audio[..], items, &footer].concat()).expect("write");
    assert_eq!(decode_all(&path), clean, "the tag is not read as audio");

    for title in ["Edited", "Edited again"] {
        crate::tagio::rewrite_via_temp(&path, |handle| -> Result<(), std::io::Error> {
            let mut file = ApeFile::read_from(handle, ParseOptions::new()).expect("read");
            let mut tag = file.remove_ape().unwrap_or_else(ApeTag::new);
            tag.insert(ApeItem::new("Title".into(), ItemValue::Text(title.into())).expect("item"));
            file.set_ape(tag);
            file.save_to(handle, WriteOptions::default()).expect("save");
            Ok(())
        })
        .expect("rewrite");

        assert!(
            fs::read(&path).expect("read").starts_with(&audio),
            "the audio's bytes must all still be there"
        );
        assert_eq!(decode_all(&path), clean, "the last frame must still decode");
        let tagged = crate::scanner::extract::read_tagged_file(&path).expect("read back");
        assert_eq!(
            tagged
                .primary_tag()
                .and_then(|t| t.title().map(|s| s.into_owned())),
            Some(title.to_string())
        );
    }
}

/// lofty's generic tag holds neither of these for APEv2, and every reader goes
/// through the generic tag.
#[test]
fn a_cover_and_a_rating_in_an_apev2_tag_reach_the_readers() {
    use lofty::ape::{ApeFile, ApeItem, ApeTag};
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::tag::ItemValue;

    const FRONT: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRfront";
    const BACK: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRback";
    let picture = |kind, data: &'static [u8]| {
        Picture::unchecked(data.to_vec())
            .pic_type(kind)
            .mime_type(MimeType::Png)
            .build()
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.ape");
    fs::copy(fixture("s16_stereo_c2000.ape"), &path).expect("copy");
    crate::tagio::rewrite_via_temp(&path, |handle| -> Result<(), std::io::Error> {
        let mut file = ApeFile::read_from(handle, ParseOptions::new()).expect("read");
        let mut tag = ApeTag::new();
        tag.insert(ApeItem::new("Title".into(), ItemValue::Text("T".into())).expect("item"));
        tag.insert(
            ApeItem::new(
                crate::scanner::extract::APE_RATING_KEY.into(),
                ItemValue::Text("80".into()),
            )
            .expect("item"),
        );
        // In the order a generic edit merges them, the back one first.
        let (remainder, mut generic) = tag.split_tag();
        generic.push_picture(picture(PictureType::CoverBack, BACK));
        generic.push_picture(picture(PictureType::CoverFront, FRONT));
        file.set_ape(remainder.merge_tag(generic));
        file.save_to(handle, WriteOptions::default()).expect("save");
        Ok(())
    })
    .expect("rewrite");

    let tagged = crate::scanner::extract::read_tagged_file(&path).expect("read back");
    let tag = tagged.primary_tag().expect("tag");
    assert_eq!(crate::scanner::extract::extract_rating(tag), Some(204));
    assert_eq!(tag.picture_count(), 2);
    assert_eq!(tag.pictures()[0].pic_type(), PictureType::CoverFront);
    assert_eq!(tag.pictures()[0].data(), FRONT);
    assert_eq!(tag.pictures()[1].data(), BACK);

    let extra = crate::scanner::extra_tags::read_extra_tags(&path).expect("extra tags");
    assert!(
        extra.is_empty(),
        "no custom column for any of it: {extra:?}"
    );
}

/// Taggers disagree on the case of these keys; APEv2 says they do not differ.
#[test]
fn replay_gain_is_read_whatever_the_case_of_its_key() {
    use lofty::ape::ApeItem;
    use lofty::tag::ItemValue;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("t.wv");
    fs::copy(fixture("s16_stereo.wv"), &path).expect("copy");
    let mut handle = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open");
    let mut file =
        lofty::wavpack::WavPackFile::read_from(&mut handle, ParseOptions::new()).expect("read");
    let mut tag = lofty::ape::ApeTag::new();
    tag.insert(
        ApeItem::new(
            "replaygain_track_gain".into(),
            ItemValue::Text("-3.25 dB".into()),
        )
        .expect("item"),
    );
    file.set_ape(tag);
    file.save_to(&mut handle, WriteOptions::default())
        .expect("save");
    drop(handle);

    let tagged = crate::scanner::extract::read_tagged_file(&path).expect("read back");
    let gain = crate::scanner::replay_gain::extract_replay_gain(tagged.primary_tag().expect("tag"));
    assert_eq!(gain.track_gain_db, Some(-3.25));
}
