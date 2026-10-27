use super::*;

/// A minimal family-0 `OpusHead`: magic, version, channels,
/// pre-skip, input rate, output gain, family.
fn head(channels: u8, gain: i16, family: u8) -> Vec<u8> {
    let mut out = b"OpusHead".to_vec();
    out.push(1); // version
    out.push(channels);
    out.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&gain.to_le_bytes());
    out.push(family);
    out
}

#[test]
fn a_stereo_header_is_one_coupled_stream() {
    let (parsed, mapping) = parse_head(&head(2, 0, 0)).expect("parse");
    assert_eq!(parsed.channel_count, 2);
    assert_eq!(mapping.streams, 1);
    assert_eq!(mapping.coupled_streams, 1);
    assert_eq!(mapping.mapping, vec![0, 1]);
}

#[test]
fn a_mono_header_is_one_uncoupled_stream() {
    let (_, mapping) = parse_head(&head(1, 0, 0)).expect("parse");
    assert_eq!(mapping.streams, 1);
    assert_eq!(mapping.coupled_streams, 0);
    assert_eq!(mapping.mapping, vec![0]);
}

#[test]
fn the_output_gain_is_read_as_signed() {
    let (parsed, _) = parse_head(&head(2, -512, 0)).expect("parse");
    assert_eq!(parsed.output_gain, -512, "-2 dB in Q7.8");
}

/// Family 1 carries its layout in a table after the header, and
/// the table has to be there.
#[test]
fn a_surround_header_reads_its_mapping_table() {
    let mut bytes = head(6, 0, 1);
    bytes.push(4); // streams
    bytes.push(2); // coupled
    bytes.extend_from_slice(&[0, 4, 1, 2, 3, 5]);
    let (_, mapping) = parse_head(&bytes).expect("parse");
    assert_eq!(mapping.streams, 4);
    assert_eq!(mapping.coupled_streams, 2);
    assert_eq!(mapping.mapping, vec![0, 4, 1, 2, 3, 5]);
}

/// Everything that reaches libopus is checked first: a header that
/// lies about its own size is a crash surface, not a decode error
/// to discover later.
#[test]
fn a_truncated_mapping_table_is_refused() {
    let mut bytes = head(6, 0, 1);
    bytes.push(4);
    bytes.push(2);
    bytes.extend_from_slice(&[0, 4]); // four bytes short
    assert!(parse_head(&bytes).is_err());
}

#[test]
fn an_impossible_stream_layout_is_refused() {
    for (streams, coupled) in [(0u8, 0u8), (2, 3)] {
        let mut bytes = head(2, 0, 255);
        bytes.push(streams);
        bytes.push(coupled);
        bytes.extend_from_slice(&[0, 1]);
        assert!(
            parse_head(&bytes).is_err(),
            "{streams} streams / {coupled} coupled should be refused"
        );
    }
}

#[test]
fn family_zero_refuses_more_than_two_channels() {
    assert!(parse_head(&head(3, 0, 0)).is_err());
}

#[test]
fn ambisonics_and_unknown_families_are_refused() {
    for family in [2u8, 3, 7, 128] {
        let mut bytes = head(2, 0, family);
        bytes.push(1);
        bytes.push(1);
        bytes.extend_from_slice(&[0, 1]);
        assert!(parse_head(&bytes).is_err(), "family {family}");
    }
}

#[test]
fn a_header_that_is_not_one_is_refused() {
    assert!(parse_head(b"").is_err());
    assert!(parse_head(b"OpusTags\x01\x02").is_err());
    let short = &head(2, 0, 0)[..18];
    assert!(parse_head(short).is_err(), "one byte short of a header");
}

#[test]
fn a_future_major_version_is_refused_but_a_minor_one_is_not() {
    let mut newer = head(2, 0, 0);
    newer[8] = 0x0F; // minor bump, still major 0
    assert!(parse_head(&newer).is_ok());
    newer[8] = 0x10;
    assert!(parse_head(&newer).is_err());
}

#[test]
fn a_header_with_no_channels_is_refused() {
    assert!(parse_head(&head(0, 0, 0)).is_err());
}

/// The bug that would otherwise ship: 312 frames of encoder
/// priming played at the head of every Opus track.
///
/// `symphonia-format-ogg` reports a per-packet discard of zero for
/// Opus — unlike Vorbis, where the reader does the counting — so
/// this decoder counts the pre-skip itself. Measured end to end
/// before this existed: a 3 s file decoded to 144 312 frames
/// instead of 144 000.
#[test]
fn the_encoder_priming_is_dropped_from_the_head_of_the_stream() {
    let mut params = AudioCodecParameters::new();
    params.codec = CODEC_ID_OPUS;
    params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
    let mut decoder =
        OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

    // A bare TOC selecting a 20 ms stereo CELT frame: 960 frames
    // of decoded audio per packet, and the header above declares a
    // 312-frame pre-skip.
    let packet = || {
        symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [0b1111_1100u8].as_slice())
    };

    let first = decoder.decode(&packet()).expect("decode").frames();
    assert_eq!(
        first,
        960 - 312,
        "the first packet is short by the pre-skip"
    );

    let second = decoder.decode(&packet()).expect("decode").frames();
    assert_eq!(second, 960, "and nothing is dropped after that");
}

/// A pre-skip longer than one packet is spent across as many as it
/// takes, rather than clamped to the first.
#[test]
fn a_priming_longer_than_a_packet_spans_several() {
    let mut bytes = head(2, 0, 0);
    // 1500 frames of pre-skip: one 960-frame packet and part of
    // the next.
    bytes[10..12].copy_from_slice(&1500u16.to_le_bytes());
    let mut params = AudioCodecParameters::new();
    params.codec = CODEC_ID_OPUS;
    params.extra_data = Some(bytes.into_boxed_slice());
    let mut decoder =
        OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

    let packet = || {
        symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [0b1111_1100u8].as_slice())
    };
    assert_eq!(decoder.decode(&packet()).expect("decode").frames(), 0);
    assert_eq!(
        decoder.decode(&packet()).expect("decode").frames(),
        960 - 540
    );
    assert_eq!(decoder.decode(&packet()).expect("decode").frames(), 960);
}

/// A seek lands in the middle of a stream, where there is no
/// priming to drop. Re-applying it would silently eat real audio
/// on every seek.
#[test]
fn a_reset_does_not_re_apply_the_priming() {
    let mut params = AudioCodecParameters::new();
    params.codec = CODEC_ID_OPUS;
    params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
    let mut decoder =
        OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

    let packet = || {
        symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [0b1111_1100u8].as_slice())
    };
    decoder.decode(&packet()).expect("decode");
    decoder.reset();
    assert_eq!(
        decoder.decode(&packet()).expect("decode").frames(),
        960,
        "nothing is owed after a seek"
    );
}

/// Gapless off means gapless off: a caller that has asked for
/// every decoded frame gets the priming too, rather than a
/// silently shortened stream.
#[test]
fn priming_is_kept_when_gapless_is_disabled() {
    let mut params = AudioCodecParameters::new();
    params.codec = CODEC_ID_OPUS;
    params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
    let opts = AudioDecoderOptions::default().gapless(false);
    let mut decoder = OpusDecoder::try_new(&params, &opts).expect("decoder");

    let packet = symphonia::core::packet::Packet::new(
        0,
        0.into(),
        960u64.into(),
        [0b1111_1100u8].as_slice(),
    );
    assert_eq!(decoder.decode(&packet).expect("decode").frames(), 960);
}

/// The end-to-end shape, against the real library: build a decoder
/// from a header we wrote, hand it a packet libopus produced, and
/// get 48 kHz stereo frames back.
#[test]
fn a_real_packet_decodes_through_libopus() {
    let mut params = AudioCodecParameters::new();
    params.codec = CODEC_ID_OPUS;
    params.extra_data = Some(head(2, 0, 0).into_boxed_slice());

    let mut decoder =
        OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("a stereo decoder");

    // The shortest legal packet: a TOC byte selecting CELT
    // fullband 20 ms stereo, one frame, with no payload. libopus
    // decodes it as silence rather than refusing it.
    let toc = 0b1111_1100u8; // config 31, stereo, code 0
    let packet = symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [toc].as_slice());
    let decoded = decoder
        .decode(&packet)
        .expect("libopus should accept a bare TOC");
    assert_eq!(decoded.spec().rate(), OPUS_RATE);
    assert_eq!(decoded.spec().channels().count(), 2);
    assert!(decoded.frames() > 0, "a 20 ms frame is 960 samples");
}

#[test]
fn a_corrupt_packet_is_an_error_and_leaves_no_audio_behind() {
    let mut params = AudioCodecParameters::new();
    params.codec = CODEC_ID_OPUS;
    params.extra_data = Some(head(2, 0, 0).into_boxed_slice());
    let mut decoder =
        OpusDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");

    // libopus reads an empty packet as packet loss and would
    // hand back concealment audio, so it is refused before it gets
    // there rather than being allowed to invent a frame.
    let empty = symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [].as_slice());
    assert!(
        decoder.decode(&empty).is_err(),
        "an empty packet is not one"
    );

    // And a packet that is data, but not Opus data.
    let junk =
        symphonia::core::packet::Packet::new(0, 0.into(), 960u64.into(), [0xFFu8; 3].as_slice());
    assert!(decoder.decode(&junk).is_err(), "a malformed packet");
    assert_eq!(
        decoder.last_decoded().frames(),
        0,
        "a failed decode must not leave the previous packet's audio readable"
    );
}
