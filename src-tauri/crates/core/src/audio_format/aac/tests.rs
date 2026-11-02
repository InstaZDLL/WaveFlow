use std::fs::File;
use std::path::{Path, PathBuf};

use lofty::file::{FileType, TaggedFileExt};
use symphonia::core::audio::layouts::{CHANNEL_LAYOUT_2P1, CHANNEL_LAYOUT_5P1, CHANNEL_LAYOUT_7P1};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::*;
use crate::audio_format::opus::codecs;
use crate::scanner::extract::{decoder_channels, probe_playable_stream, read_tagged_file};

/// FFmpeg's native encoder, 0.4 s at 48 kHz, one tone per channel in
/// FFmpeg's channel order. `-aac_pns 0`: a tone has nothing to substitute,
/// and noise would make the planes differ between decoders.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/aac")
        .join(name)
}

const FL: f32 = 440.0;
const FR: f32 = 554.0;
const FC: f32 = 659.0;
const LFE: f32 = 110.0;
const BL: f32 = 784.0;
const BR: f32 = 880.0;
const SL: f32 = 988.0;
const SR: f32 = 1109.0;

fn open(path: &Path) -> (Box<dyn FormatReader>, AudioCodecParameters) {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(File::open(path).unwrap()), Default::default()),
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .unwrap();
    let params = format
        .default_track(TrackType::Audio)
        .unwrap()
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap()
        .clone();
    (format, params)
}

/// The decoded layout and one plane per channel, through the registry
/// playback, analysis and the scanner all open streams with.
fn decode_all(path: &Path) -> (Channels, Vec<Vec<f32>>) {
    let (mut format, params) = open(path);
    let mut decoder = codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .unwrap();
    let mut layout = None;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    while let Ok(Some(packet)) = format.next_packet() {
        let decoded = decoder.decode(&packet).unwrap();
        let spec = decoded.spec().clone();
        planes.resize(spec.channels().count(), Vec::new());
        let mut interleaved = vec![0.0f32; decoded.samples_interleaved()];
        decoded.copy_to_slice_interleaved(&mut interleaved);
        for frame in interleaved.chunks(planes.len()) {
            for (plane, sample) in planes.iter_mut().zip(frame) {
                plane.push(*sample);
            }
        }
        layout = Some(spec.channels().clone());
    }
    (layout.unwrap(), planes)
}

/// The power of `plane` at `freq`, by Goertzel.
fn power_at(plane: &[f32], freq: f32) -> f32 {
    let coeff = 2.0 * (2.0 * std::f32::consts::PI * freq / 48_000.0).cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for sample in plane {
        let s = sample + coeff * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

fn assert_each_speaker_holds_its_tone(planes: &[Vec<f32>], tones: &[f32]) {
    assert_eq!(planes.len(), tones.len());
    for (index, (plane, expected)) in planes.iter().zip(tones).enumerate() {
        let strongest = tones
            .iter()
            .copied()
            .max_by(|a, b| power_at(plane, *a).total_cmp(&power_at(plane, *b)))
            .unwrap();
        assert_eq!(strongest, *expected, "channel {index}");
    }
}

#[test]
fn five_point_one_comes_out_with_every_speaker_in_its_place() {
    for file in ["fivepointone.m4a", "fivepointone.aac"] {
        let (layout, planes) = decode_all(&fixture(file));
        assert_eq!(layout, CHANNEL_LAYOUT_5P1, "{file}");
        assert_each_speaker_holds_its_tone(&planes, &[FL, FR, FC, LFE, BL, BR]);
    }
}

#[test]
fn seven_point_one_comes_out_with_every_speaker_in_its_place() {
    let (layout, planes) = decode_all(&fixture("sevenpointone.m4a"));
    assert_eq!(layout, CHANNEL_LAYOUT_7P1);
    assert_each_speaker_holds_its_tone(&planes, &[FL, FR, FC, LFE, BL, BR, SL, SR]);
}

/// 2.1 has no channel configuration: FFmpeg writes a program config
/// element, which symphonia refuses and which is where the speakers come
/// from.
#[test]
fn a_layout_from_a_program_config_element_names_its_speakers() {
    let (layout, planes) = decode_all(&fixture("twopointone.m4a"));
    assert_eq!(layout, CHANNEL_LAYOUT_2P1);
    assert_each_speaker_holds_its_tone(&planes, &[FL, FR, LFE]);
}

/// FFmpeg's PCE for 5.1 with side surrounds files the LFE as a side channel,
/// so nothing can name the speakers: the channels stay discrete, in the
/// order they were encoded, and all six are counted.
#[test]
fn a_program_config_element_that_names_no_speakers_stays_discrete() {
    let (layout, planes) = decode_all(&fixture("sidefive.m4a"));
    assert_eq!(layout, Channels::Discrete(6));
    assert_each_speaker_holds_its_tone(&planes, &[FL, FR, FC, LFE, SL, SR]);
}

/// What symphonia accepted before this decoder existed stays symphonia's,
/// so nobody's stereo library changes decoder.
#[test]
fn symphonia_keeps_what_it_accepts_and_refuses_the_rest_to_the_extended_decoder() {
    let opts = AudioDecoderOptions::default();
    let (_, stereo) = open(&fixture("stereo.m4a"));
    assert!(matches!(
        AacDecoder::try_new(&stereo, &opts).unwrap(),
        AacDecoder::Symphonia { .. }
    ));
    let (_, surround) = open(&fixture("fivepointone.m4a"));
    assert!(matches!(
        AacDecoder::try_new(&surround, &opts).unwrap(),
        AacDecoder::Extended(_)
    ));
}

/// The registry is what keeps the three openers from disagreeing, and it
/// has two builds: this file is compiled with either.
#[test]
fn the_registry_hands_aac_to_this_decoder() {
    let registered = codecs().get_audio_decoder(CODEC_ID_AAC).unwrap();
    let (_, surround) = open(&fixture("fivepointone.m4a"));
    assert_eq!(registered.codec.info.short_name, "aac");
    assert!(codecs()
        .make_audio_decoder(&surround, &AudioDecoderOptions::default())
        .is_ok());
}

#[test]
fn the_scanner_and_the_analysis_read_what_playback_plays() {
    let path = fixture("fivepointone.m4a");
    let probed = probe_playable_stream(&path).unwrap();
    assert_eq!(probed.channels, Some(6));
    assert_eq!(probed.codec.as_deref(), Some("AAC"));
    assert!(crate::analysis::analyze_file(&path).is_ok());
}

/// The count lofty gets wrong on this family: it reads 7 for 7.1 and 2 for
/// a stream with a program config element.
#[test]
fn the_decoder_counts_the_channels_for_the_scanner() {
    let count = |name: &str| {
        let path = fixture(name);
        let file_type = read_tagged_file(&path).unwrap().file_type();
        decoder_channels(&path, file_type).unwrap()
    };
    assert_eq!(count("stereo.m4a"), Some(2));
    assert_eq!(count("fivepointone.m4a"), Some(6));
    assert_eq!(count("fivepointone.aac"), Some(6));
    assert_eq!(count("sevenpointone.m4a"), Some(8));
    assert_eq!(count("twopointone.m4a"), Some(3));
    assert_eq!(count("sidefive.m4a"), Some(6));
    // The layout of these two arrives in-band, so a packet has to be decoded.
    assert_eq!(count("twopointone.aac"), Some(3));
    assert_eq!(count("sixpointone.aac"), Some(7));
}

/// In an MP4 the PCE names the speakers at the open. In ADTS it has to be
/// read from the first packet, for the same file to be the same 2.1.
#[test]
fn an_in_band_program_config_element_names_the_same_speakers() {
    let (layout, planes) = decode_all(&fixture("twopointone.aac"));
    assert_eq!(layout, CHANNEL_LAYOUT_2P1);
    assert_each_speaker_holds_its_tone(&planes, &[FL, FR, LFE]);
}

/// AAC-LC with SBR declared after a sync extension: 22.05 kHz core, 44.1 kHz
/// out. symphonia reads object type 2 and would play the core alone.
#[test]
fn sbr_declared_the_backward_compatible_way_is_not_left_to_symphonia() {
    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_AAC)
        .with_extra_data(vec![0x13, 0x90, 0x56, 0xE5, 0xA0].into_boxed_slice());
    let decoder = AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    assert!(matches!(decoder, AacDecoder::Extended(_)));
    assert_eq!(decoder.codec_params().sample_rate, Some(44_100));
}

/// A stream declaring stereo, with a program config element in its packets:
/// symphonia opens it, then refuses the packet as unsupported, which a caller
/// takes as fatal. The stream moves to the other decoder and no audio is
/// lost, since nothing has read a format yet.
#[test]
fn what_symphonia_refuses_at_the_first_packet_moves_to_the_other_decoder() {
    let (mut format, _) = open(&fixture("twopointone.aac"));
    let mut decoder = declared_stereo();
    assert!(matches!(decoder, AacDecoder::Symphonia { .. }));

    let first = format.next_packet().unwrap().unwrap();
    assert_eq!(
        decoder.decode(&first).unwrap().spec().channels(),
        &CHANNEL_LAYOUT_2P1
    );
    assert!(matches!(decoder, AacDecoder::Extended(_)));
}

/// Further in, a caller holds a format that no longer applies.
#[test]
fn what_symphonia_refuses_mid_stream_announces_the_new_format() {
    let (mut stereo, _) = open(&fixture("stereo.m4a"));
    let mut decoder = declared_stereo();
    decoder
        .decode(&stereo.next_packet().unwrap().unwrap())
        .unwrap();

    let (mut surround, _) = open(&fixture("twopointone.aac"));
    let packet = surround.next_packet().unwrap().unwrap();
    assert!(matches!(decoder.decode(&packet), Err(Error::ResetRequired)));
    let next = surround.next_packet().unwrap().unwrap();
    assert_eq!(decoder.decode(&next).unwrap().spec().channels().count(), 3);
}

fn declared_stereo() -> AacDecoder {
    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_AAC)
        .with_sample_rate(48_000)
        .with_channels(CHANNEL_LAYOUT_STEREO);
    AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap()
}

/// Nothing declares the layout and no packet decodes: the count is unknown
/// because the stream does not play, which is a refusal and not a shrug.
#[test]
fn a_stream_of_which_no_packet_decodes_is_refused_for_the_scanner() {
    let mut bytes = std::fs::read(fixture("twopointone.aac")).unwrap();
    let mut at = 0;
    while at + 7 <= bytes.len() {
        let length = (usize::from(bytes[at + 3] & 0x03) << 11)
            | (usize::from(bytes[at + 4]) << 3)
            | usize::from(bytes[at + 5] >> 5);
        bytes[at + 7..at + length].fill(0xFF);
        at += length;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("damaged.aac");
    std::fs::write(&path, bytes).unwrap();
    assert!(decoder_channels(&path, FileType::Aac).is_err());
    assert!(probe_playable_stream(&path).is_none());
}

/// The container says the codec, so the file is not even opened.
#[test]
fn a_format_that_names_its_codec_is_not_opened_for_the_decoders_view() {
    assert_eq!(
        decoder_channels(Path::new("missing.flac"), FileType::Flac),
        Ok(None)
    );
}

/// A `.m4a` declaring USAC: `fivepointone.m4a` with its AudioSpecificConfig
/// rewritten to object type 42. lofty still reads it, which is the defect
/// this guards against.
fn usac_file(dir: &Path) -> PathBuf {
    let mut bytes = std::fs::read(fixture("fivepointone.m4a")).unwrap();
    let at = bytes
        .windows(2)
        .position(|pair| pair == [0x11, 0xB0])
        .expect("the AudioSpecificConfig of 48 kHz AAC-LC 5.1");
    // 11111 (escape) 001010 (32 + 10) 0011 (48 kHz) 0.
    bytes[at..at + 2].copy_from_slice(&[0xF9, 0x46]);
    let path = dir.join("usac.m4a");
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn xhe_aac_is_refused_everywhere_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let path = usac_file(dir.path());
    assert!(read_tagged_file(&path).is_ok());

    let reason = decoder_channels(&path, FileType::Mp4).unwrap_err();
    assert!(reason.contains("cannot decode"), "{reason}");
    assert!(reason.contains("xHE-AAC"), "{reason}");
    assert!(probe_playable_stream(&path).is_none());
    assert!(crate::analysis::analyze_file(&path).is_err());
}

/// lofty goes by content, so the check has to: the name says nothing.
#[test]
fn xhe_aac_under_another_name_is_refused_all_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let renamed = dir.path().join("usac.mp3");
    std::fs::rename(usac_file(dir.path()), &renamed).unwrap();
    let file_type = read_tagged_file(&renamed).unwrap().file_type();
    assert!(decoder_channels(&renamed, file_type).is_err());
}

#[test]
fn a_file_without_an_audio_stream_is_refused_for_the_scanner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.mp4");
    std::fs::write(&path, b"not an mp4 at all").unwrap();
    assert!(decoder_channels(&path, FileType::Mp4).is_err());
}

/// A bad packet is reported and leaves the decoder usable: the player skips
/// it and carries on, and the format does not move.
#[test]
fn a_damaged_packet_is_skipped_and_the_stream_goes_on() {
    let (mut format, params) = open(&fixture("fivepointone.m4a"));
    let mut decoder = AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    let good = format.next_packet().unwrap().unwrap();
    decoder.decode(&good).unwrap();

    // Cut, not scrambled: all ones would read as an end element straight
    // away, which is a valid empty frame.
    let mut damaged = format.next_packet().unwrap().unwrap();
    damaged.data = damaged.data[..4].to_vec().into();
    assert!(matches!(
        decoder.decode(&damaged),
        Err(Error::DecodeError(_))
    ));
    assert_eq!(decoder.last_decoded().frames(), 0);

    let next = format.next_packet().unwrap().unwrap();
    let decoded = decoder.decode(&next).unwrap();
    assert_eq!(decoded.spec().channels(), &CHANNEL_LAYOUT_5P1);
}

/// After a seek nothing of the earlier stream may reach the next packet:
/// a reset decoder answers exactly like a new one.
#[test]
fn a_reset_forgets_the_part_of_the_stream_before_the_seek() {
    let opts = AudioDecoderOptions::default();
    let (mut format, params) = open(&fixture("fivepointone.m4a"));
    let mut played = AacDecoder::try_new(&params, &opts).unwrap();
    for _ in 0..5 {
        played
            .decode(&format.next_packet().unwrap().unwrap())
            .unwrap();
    }
    played.reset();

    let target = format.next_packet().unwrap().unwrap();
    let mut fresh = AacDecoder::try_new(&params, &opts).unwrap();
    let samples = |decoded: GenericAudioBufferRef<'_>| {
        let mut out = vec![0.0f32; decoded.samples_interleaved()];
        decoded.copy_to_slice_interleaved(&mut out);
        out
    };
    let after_reset = samples(played.decode(&target).unwrap());
    let from_scratch = samples(fresh.decode(&target).unwrap());
    assert!(!after_reset.is_empty());
    assert_eq!(after_reset, from_scratch);
}

/// Two ADTS streams whose layouts arrive in-band, one after the other: 2.1
/// then 6.1. Nothing declares the change, the decoder finds it in a packet.
fn format_change_file(dir: &Path) -> PathBuf {
    let mut bytes = std::fs::read(fixture("twopointone.aac")).unwrap();
    bytes.extend(std::fs::read(fixture("sixpointone.aac")).unwrap());
    let path = dir.join("chained.aac");
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_format_change_mid_stream_is_announced_once() {
    let dir = tempfile::tempdir().unwrap();
    let (mut format, params) = open(&format_change_file(dir.path()));
    let mut decoder = AacDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    let (mut announced, mut counts) = (0, Vec::new());
    while let Ok(Some(packet)) = format.next_packet() {
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let count = decoded.spec().channels().count();
                if counts.last() != Some(&count) {
                    counts.push(count);
                }
            }
            Err(Error::ResetRequired) => announced += 1,
            Err(err) => panic!("{err}"),
        }
    }
    assert_eq!(announced, 1);
    assert_eq!(counts, [3, 7]);
}

/// The analysis is sized by the first packet's format and cannot follow a
/// change: it keeps what it measured instead of failing the whole file.
#[test]
fn the_analysis_keeps_what_it_measured_when_the_format_changes() {
    let dir = tempfile::tempdir().unwrap();
    assert!(crate::analysis::analyze_file(&format_change_file(dir.path())).is_ok());
}

#[test]
fn every_channel_configuration_names_as_many_speakers_as_it_has_channels() {
    for (configuration, count) in [(1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (7, 8)] {
        let stream = StreamConfig::lc(aot::AAC_LC, 48_000, configuration);
        let named = speakers(&stream).unwrap_or_else(|| panic!("configuration {configuration}"));
        assert_eq!(named.count(), count, "configuration {configuration}");
    }
    assert_eq!(
        speakers(&StreamConfig::lc(aot::AAC_LC, 48_000, 6)),
        Some(CHANNEL_LAYOUT_5P1)
    );
    // Configuration 7 is what encoders write for 7.1 with side and back
    // surrounds, whatever ISO says; the bitstream decides by measure above.
    assert_eq!(
        speakers(&StreamConfig::lc(aot::AAC_LC, 48_000, 7)),
        Some(CHANNEL_LAYOUT_7P1)
    );
}

#[test]
fn a_stream_that_keeps_its_layout_in_band_names_no_speakers_yet() {
    assert_eq!(speakers(&StreamConfig::lc(aot::AAC_LC, 48_000, 0)), None);
    assert_eq!(channel_configuration(7), 0);
    assert_eq!(channel_configuration(8), 7);
}

#[cfg(feature = "playback")]
#[test]
fn playback_opens_what_the_scanner_lists() {
    use crate::playback::replay_gain::TrackGain;
    use crate::playback::stream::ActiveStream;

    for name in [
        "fivepointone.m4a",
        "fivepointone.aac",
        "sevenpointone.m4a",
        "sidefive.m4a",
    ] {
        let opened = ActiveStream::open(
            &fixture(name),
            1,
            400,
            "local".into(),
            None,
            TrackGain::default(),
            64,
            false,
        );
        assert!(opened.is_ok(), "{name}");
    }
}
