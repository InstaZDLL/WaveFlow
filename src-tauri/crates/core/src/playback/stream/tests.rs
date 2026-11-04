use super::*;

fn stereo() -> ChannelLayout {
    ChannelLayout::from_count(2)
}

/// One second of 44.1 kHz 16-bit stereo PCM, as a WAV file.
fn write_wav(path: &Path) {
    let (rate, frames, channels) = (44_100_u32, 44_100_u32, 2_u16);
    let data_len = frames * u32::from(channels) * 2;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data_len).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend((channels * 2).to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_len.to_le_bytes());
    for i in 0..frames {
        let sample = ((i % 100) as i16 - 50) * 300;
        bytes.extend(sample.to_le_bytes());
        bytes.extend(sample.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// Played to the end at another rate, a track comes out at its full
/// length: what the resampler held at the end is not lost.
#[test]
fn a_track_resampled_to_its_end_keeps_its_length() {
    use rubato::Resampler as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    let mut stream = ActiveStream::open(
        &path,
        0,
        0,
        String::new(),
        None,
        TrackGain::default(),
        64,
        false,
    )
    .unwrap();
    assert_eq!(stream.container_duration_ms, Some(1_000));
    assert_eq!(stream.codec.as_deref(), Some("PCM"));

    let (mut out, mut scratch) = (Vec::new(), Vec::new());
    while !stream
        .decode_next(&mut out, &mut scratch, 48_000, &stereo())
        .unwrap()
    {}
    // Asked again at the end, it adds nothing more.
    let at_end = out.len();
    assert!(stream
        .decode_next(&mut out, &mut scratch, 48_000, &stereo())
        .unwrap());
    assert_eq!(out.len(), at_end);

    let delay = match &stream.resampler {
        Resampler::Fft { inner, .. } => inner.output_delay(),
        Resampler::Passthrough => panic!("44.1 kHz to 48 kHz resamples"),
    };
    assert_eq!(out.len() / 2, delay + 48_000);
}

/// The tally counts every frame decoded and the bytes they came from,
/// and a seek adds to it rather than starting it again.
#[test]
fn the_packet_tally_counts_what_was_decoded_and_survives_a_seek() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    let mut stream = ActiveStream::open(
        &path,
        0,
        0,
        String::new(),
        None,
        TrackGain::default(),
        64,
        false,
    )
    .unwrap();
    assert_eq!(stream.packets_read, PacketTally::default());

    let (mut out, mut scratch) = (Vec::new(), Vec::new());
    while !stream
        .decode_next(&mut out, &mut scratch, 44_100, &stereo())
        .unwrap()
    {}
    // One second of 16-bit stereo: 44 100 frames from 176 400 bytes.
    let whole = PacketTally {
        bytes: 176_400,
        frames: 44_100,
    };
    assert_eq!(stream.packets_read, whole);

    stream.seek_ms(0);
    assert_eq!(stream.packets_read, whole);
    assert!(!stream
        .decode_next(&mut out, &mut scratch, 44_100, &stereo())
        .unwrap());
    assert!(stream.packets_read.frames > whole.frames);
    assert!(stream.packets_read.bytes > whole.bytes);
}

/// Read ahead in the background, a track decodes to exactly what the
/// file gives read directly.
#[test]
fn a_track_read_ahead_decodes_like_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    let decode_all = |mut stream: ActiveStream| {
        let (mut out, mut scratch) = (Vec::new(), Vec::new());
        while !stream
            .decode_next(&mut out, &mut scratch, 44_100, &stereo())
            .unwrap()
        {}
        out
    };
    let direct = decode_all(
        ActiveStream::open(
            &path,
            0,
            0,
            String::new(),
            None,
            TrackGain::default(),
            256,
            false,
        )
        .unwrap(),
    );
    let len = std::fs::metadata(&path).unwrap().len();
    let source = ReadAhead::spawn(File::open(&path).unwrap(), None, len).unwrap();
    let read_ahead = decode_all(
        ActiveStream::open_from_source(
            Box::new(source),
            Some("wav"),
            0,
            0,
            String::new(),
            None,
            TrackGain::default(),
        )
        .unwrap(),
    );
    assert_eq!(read_ahead.len(), 44_100 * 2);
    assert_eq!(read_ahead, direct);
}

/// The engine's own path through a file cut short: the stream says it is done
/// at the last whole frame, and a seek past it brings the end at once.
#[test]
fn a_stream_cut_short_ends_at_what_the_file_holds() {
    use ape_decoder::format::parse;

    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lossless/seek_frames_c1000.ape");
    let bytes = std::fs::read(&source).expect("read");
    let info = parse(&mut std::io::Cursor::new(bytes.clone())).expect("parse");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cut.ape");
    std::fs::write(&path, &bytes[..info.seek_byte(2) as usize + 100]).expect("write");

    let open = || {
        ActiveStream::open(
            &path,
            1,
            3_628,
            "local".to_string(),
            None,
            TrackGain::default(),
            256,
            false,
        )
        .expect("open")
    };

    let (mut out, mut scratch) = (Vec::new(), Vec::new());
    let mut stream = open();
    while !stream
        .decode_next(&mut out, &mut scratch, 44_100, &stereo())
        .expect("decode")
    {}
    assert_eq!(out.len() / 2, 2 * 73_728, "two whole frames, nothing after");

    let mut stream = open();
    out.clear();
    assert!(!stream
        .decode_next(&mut out, &mut scratch, 44_100, &stereo())
        .expect("decode"));
    stream.seek_ms(3_500);
    stream.reset_decoder();
    out.clear();
    let mut steps = 0;
    while !stream
        .decode_next(&mut out, &mut scratch, 44_100, &stereo())
        .expect("decode")
    {
        steps += 1;
        assert!(steps < 3, "the end comes at once");
    }
}

/// Skipped packets are bounded by time as well as by count: a slow codec's
/// failures would otherwise keep the player's commands waiting for a minute.
#[test]
fn a_run_of_bad_packets_gives_way_to_the_commands() {
    use std::time::{Duration, Instant};

    let just_now = Instant::now();
    assert!(!should_give_way(1, just_now));
    assert!(should_give_way(MAX_SKIPPED_PACKETS, just_now));
    let a_while_ago = just_now - Duration::from_secs(1);
    assert!(should_give_way(1, a_while_ago));
}

#[test]
fn network_path_heuristic() {
    // Windows UNC + Linux gvfs mounts are network.
    assert!(path_str_is_network(r"\\nas\music\track.flac"));
    assert!(path_str_is_network(
        "/run/user/1000/gvfs/smb-share:server=nas,share=music/track.flac"
    ));
    // Extended-UNC is still network.
    assert!(path_str_is_network(r"\\?\UNC\nas\music\track.flac"));
    // Plain local paths are not.
    assert!(!path_str_is_network("/home/user/Music/track.flac"));
    assert!(!path_str_is_network(r"C:\Users\me\Music\track.flac"));
    // \\?\ and \\.\ namespace prefixes are local, not network.
    assert!(!path_str_is_network(r"\\?\C:\Users\me\Music\track.flac"));
    assert!(!path_str_is_network(r"\\.\C:\Users\me\Music\track.flac"));
    // The UNC exception is \\?\ only — \\.\UNC\ stays local (device).
    assert!(!path_str_is_network(r"\\.\UNC\nas\music\track.flac"));
}

mod extent_tests {
    use super::super::{frames_to_ticks, StreamExtent};

    #[test]
    fn frames_convert_to_the_track_time_base() {
        // FLAC: the time base is the sample rate's reciprocal.
        assert_eq!(frames_to_ticks(4_608, 44_100, 1, 44_100), Some(4_608));
        // A millisecond time base at 48 kHz: 4 608 frames are 96 ms.
        assert_eq!(frames_to_ticks(4_608, 48_000, 1, 1_000), Some(96));
        assert_eq!(frames_to_ticks(4_608, 0, 1, 1_000), None);
    }

    #[test]
    fn a_millisecond_time_base_measures_in_milliseconds() {
        // 2 s declared as 2 000 ms ticks, a lost 96 ms packet.
        let mut extent = StreamExtent::new(0, Some(2_000), Some(96));
        extent.read(1_808, 96);
        assert!(extent.ends_within_last_packet());
        extent.read(1_000, 96);
        assert!(!extent.ends_within_last_packet());
    }

    #[test]
    fn the_seed_gives_way_to_the_first_real_packet() {
        // A 16 384-frame fallback, then 4 608-frame packets: the window is
        // one real packet, not the fallback.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(16_384));
        extent.read(0, 4_608);
        extent.read(70_000, 4_608);
        assert!(!extent.ends_within_last_packet());
        extent.read(82_944, 4_608);
        assert!(extent.ends_within_last_packet());
    }

    #[test]
    fn stray_bytes_after_the_last_frame_end_the_track() {
        // The report's file: 88 200 frames declared, 4 608-frame packets,
        // the last one lost to the trailing bytes.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.read(82_944, 4_608);
        assert!(extent.ends_within_last_packet());
    }

    #[test]
    fn a_file_cut_short_is_still_an_error() {
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.read(40_000, 4_608);
        assert!(!extent.ends_within_last_packet());
        // Nothing declared, nothing to measure against.
        let mut unknown = StreamExtent::new(0, None, Some(4_608));
        unknown.read(83_000, 4_608);
        assert!(!unknown.ends_within_last_packet());
    }

    #[test]
    fn decoding_well_past_the_declared_end_is_not_its_end() {
        // Declares 88 200 frames, holds more, and is cut at 200 000.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.read(195_392, 4_608);
        assert!(!extent.ends_within_last_packet());
        // Just past it, within a packet, still is.
        extent.read(86_000, 4_608);
        assert!(extent.ends_within_last_packet());
    }

    #[test]
    fn a_seek_into_the_tail_counts_from_where_it_landed() {
        // No packet read yet: the declared maximum is the measure.
        let mut extent = StreamExtent::new(0, Some(88_200), Some(4_608));
        extent.seeked(86_000);
        assert!(extent.ends_within_last_packet());
        // And with no measure at all, the error stands.
        let mut blind = StreamExtent::new(0, Some(88_200), None);
        blind.seeked(86_000);
        assert!(!blind.ends_within_last_packet());
    }
}

/// A WAV file of one second, a distinct DC level per channel, `mask` naming
/// them in `WAVEFORMATEXTENSIBLE`.
fn write_surround_wav(path: &Path, mask: u32, levels: &[i16]) {
    let channels = levels.len() as u16;
    let (rate, frames) = (48_000_u32, 4_800_u32);
    let data_len = frames * u32::from(channels) * 2;
    let mut fmt = Vec::new();
    fmt.extend(0xFFFE_u16.to_le_bytes());
    fmt.extend(channels.to_le_bytes());
    fmt.extend(rate.to_le_bytes());
    fmt.extend((rate * u32::from(channels) * 2).to_le_bytes());
    fmt.extend((channels * 2).to_le_bytes());
    fmt.extend(16_u16.to_le_bytes());
    fmt.extend(22_u16.to_le_bytes());
    fmt.extend(16_u16.to_le_bytes());
    fmt.extend(mask.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE_PCM
    fmt.extend([
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
        0x71,
    ]);
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((4 + 8 + fmt.len() as u32 + 8 + data_len).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend((fmt.len() as u32).to_le_bytes());
    bytes.extend(&fmt);
    bytes.extend(b"data");
    bytes.extend(data_len.to_le_bytes());
    for _ in 0..frames {
        for level in levels {
            bytes.extend(level.to_le_bytes());
        }
    }
    std::fs::write(path, bytes).unwrap();
}

/// The level of each output channel in the middle of a file decoded to
/// `dst`, as a fraction of the 1000-step unit the levels are written in.
fn decoded_levels(path: &Path, dst: &ChannelLayout) -> Vec<f32> {
    let mut stream = ActiveStream::open(
        path,
        0,
        0,
        String::new(),
        None,
        TrackGain::default(),
        64,
        false,
    )
    .unwrap();
    let (mut out, mut scratch) = (Vec::new(), Vec::new());
    while !stream
        .decode_next(&mut out, &mut scratch, 48_000, dst)
        .unwrap()
    {}
    let channels = dst.channels();
    let middle = out.len() / channels / 2 * channels;
    out[middle..middle + channels]
        .iter()
        .map(|s| (s * 32_768.0 / 1_000.0 * 100.0).round() / 100.0)
        .collect()
}

/// The WAVE mask decides, not the count: a 5.1 written with side channels
/// lands on the side speakers of a 7.1 output, one with back channels on the
/// back ones.
#[test]
fn a_file_names_its_own_surrounds() {
    let dir = tempfile::tempdir().unwrap();
    let levels = [1_000, 2_000, 3_000, 4_000, 5_000, 6_000];
    let wide = ChannelLayout::from_count(8);

    let side = dir.path().join("side.wav");
    write_surround_wav(&side, 0x60F, &levels);
    assert_eq!(
        decoded_levels(&side, &wide),
        [1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 5.0, 6.0]
    );

    let back = dir.path().join("back.wav");
    write_surround_wav(&back, 0x3F, &levels);
    assert_eq!(
        decoded_levels(&back, &wide),
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 0.0, 0.0]
    );

    // On a 5.1 output they are the same speakers.
    let five_one = ChannelLayout::from_count(6);
    assert_eq!(
        decoded_levels(&side, &five_one),
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
    );
    assert_eq!(
        decoded_levels(&back, &five_one),
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
    );
}

/// An output that wants its channels in another order gets each one on its
/// own speaker, from the decoder on.
#[test]
fn an_output_in_alsa_order_receives_the_channels_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("five-one.wav");
    write_surround_wav(&path, 0x3F, &[1_000, 2_000, 3_000, 4_000, 5_000, 6_000]);
    assert_eq!(
        decoded_levels(&path, &ChannelLayout::alsa_default(6)),
        [1.0, 2.0, 5.0, 6.0, 3.0, 4.0]
    );
}

/// The 7.1 on 5.1 of the card, through a real stream.
#[test]
fn seven_one_on_five_one_folds_the_sides_in() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("seven-one.wav");
    write_surround_wav(
        &path,
        0x63F,
        &[1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000, 8_000],
    );
    let out = decoded_levels(&path, &ChannelLayout::from_count(6));
    // The heaviest channel sums to 1.414, which scales the whole mix by
    // 0.707, leaving each surround at 0.5 + 0.5 of its two sources.
    assert_eq!(out[..4], [0.71, 1.41, 2.12, 2.83]);
    assert!((out[4] - (5.0 + 7.0) * 0.5).abs() < 0.02, "{out:?}");
    assert!((out[5] - (6.0 + 8.0) * 0.5).abs() < 0.02, "{out:?}");
}

#[test]
fn a_dsf_three_one_is_not_read_as_a_quad() {
    use crate::audio_format::dsd::parser::{DsdChannels, DsdContainer, DsdLayout, DsdSpeakers};
    use crate::playback::channels::Speaker::*;
    let base = |speakers, channels| DsdLayout {
        container: DsdContainer::Dsf,
        channels,
        speakers,
        sample_rate_hz: 2_822_400,
        samples_per_channel: 0,
        data_offset: 0,
        data_len_bytes: 0,
        block_interleave: Some(4096),
        lsb_first: true,
    };
    assert_eq!(
        dsd::dsd_channel_layout(&base(DsdSpeakers::DsfType(5), DsdChannels::Multi(4))).speakers(),
        [FrontLeft, FrontRight, FrontCenter, Lfe]
    );
    assert_eq!(
        dsd::dsd_channel_layout(&base(DsdSpeakers::DsfType(4), DsdChannels::Multi(4))),
        ChannelLayout::from_count(4)
    );
    assert_eq!(
        dsd::dsd_channel_layout(&base(DsdSpeakers::DsfType(7), DsdChannels::Multi(6))),
        ChannelLayout::from_count(6)
    );
}

/// A DFF lists its channels: the order is the file's, whatever it is.
#[test]
fn a_dff_is_read_in_the_order_it_names() {
    use crate::audio_format::dsd::parser::{DsdChannels, DsdContainer, DsdLayout, DsdSpeakers};
    use crate::playback::channels::Speaker::*;
    let base = |speakers, channels| DsdLayout {
        container: DsdContainer::Dff,
        channels,
        speakers,
        sample_rate_hz: 2_822_400,
        samples_per_channel: 0,
        data_offset: 0,
        data_len_bytes: 0,
        block_interleave: None,
        lsb_first: false,
    };
    let odd = DsdSpeakers::DffIds(vec![
        *b"C   ", *b"LFE ", *b"MLFT", *b"MRGT", *b"LS  ", *b"RS  ",
    ]);
    assert_eq!(
        dsd::dsd_channel_layout(&base(odd, DsdChannels::Multi(6))).speakers(),
        [FrontCenter, Lfe, FrontLeft, FrontRight, BackLeft, BackRight]
    );
    // An identifier we do not know, or none at all: the count.
    let unknown = DsdSpeakers::DffIds(vec![*b"XXXX", *b"YYYY"]);
    assert_eq!(
        dsd::dsd_channel_layout(&base(unknown, DsdChannels::Stereo)),
        ChannelLayout::from_count(2)
    );
    let unnamed = DsdSpeakers::DffIds(Vec::new());
    assert_eq!(
        dsd::dsd_channel_layout(&base(unnamed, DsdChannels::Multi(6))),
        ChannelLayout::from_count(6)
    );
}

fn open_tone(path: &Path) -> ActiveStream {
    ActiveStream::open(
        path,
        0,
        0,
        String::new(),
        None,
        TrackGain::default(),
        64,
        false,
    )
    .unwrap()
}

fn decode_to_end(stream: &mut ActiveStream, rate: u32) -> Vec<f32> {
    let (mut out, mut scratch) = (Vec::new(), Vec::new());
    while !stream
        .decode_next(&mut out, &mut scratch, rate, &stereo())
        .unwrap()
    {}
    out
}

fn shift(speed: f32, keep_pitch: bool, semitones: f32) -> TimeShift {
    TimeShift {
        speed,
        keep_pitch,
        semitones,
    }
}

/// At 1× with no transposition the audio is what it always was, to the bit,
/// and no stretcher exists: exclusive output and DoP stay bit-perfect.
#[test]
fn at_one_times_nothing_is_built_and_nothing_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    for rate in [44_100, 48_000] {
        let reference = decode_to_end(&mut open_tone(&path), rate);
        let mut stream = open_tone(&path);
        stream.set_time_shift(shift(1.0, true, 0.0));
        stream.set_output_gain(0.5);
        assert_eq!(decode_to_end(&mut stream, rate), reference);
        assert!(stream.stretcher.is_none());
        assert_eq!(
            stream.output_delay_frames(),
            stream.resampler.output_delay_frames()
        );
    }
}

/// With the pitch kept the resampler keeps the true rate, and the length is the
/// source's at the speed: to the frame with no resampling.
#[test]
fn keeping_the_pitch_gives_the_length_of_the_speed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    for speed in [0.75_f32, 1.25, 2.0] {
        let mut stream = open_tone(&path);
        stream.set_time_shift(shift(speed, true, 0.0));
        let out = decode_to_end(&mut stream, 44_100);
        assert!(matches!(stream.resampler, Resampler::Passthrough));
        assert!(stream.stretcher.is_some());
        let expected = (44_100.0 / f64::from(speed)).round() as usize;
        assert_eq!(out.len() / 2, expected, "speed {speed}");
        assert_eq!(stream.output_delay_frames(), 0);
    }
}

/// Through the resampler too, whose delay is dropped on the way in: a stretched
/// stream starts on its first frame, for the gapless hand-off and for AutoMix.
#[test]
fn keeping_the_pitch_at_another_rate_adds_no_delay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    let mut stream = open_tone(&path);
    stream.set_time_shift(shift(1.25, true, 0.0));
    let out = decode_to_end(&mut stream, 48_000);
    assert!(stream.resampler.output_delay_frames() > 0);
    assert_eq!(stream.output_delay_frames(), 0);
    assert_eq!(out.len() / 2, 38_400);
}

/// The other mode is the fake source rate it always was, with no stretcher.
#[test]
fn a_speed_that_moves_the_pitch_goes_through_the_resampler() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    let mut stream = open_tone(&path);
    stream.set_time_shift(shift(1.25, false, 0.0));
    let out = decode_to_end(&mut stream, 44_100);
    assert!(stream.stretcher.is_none());
    assert!(matches!(stream.resampler, Resampler::Fft { .. }));
    let delay = stream.resampler.output_delay_frames() as usize;
    let expected = delay + (44_100.0_f64 / 1.25).round() as usize;
    assert!(out.len() / 2 >= expected - 2 && out.len() / 2 <= expected + 2);
}

/// `seconds` of a sine as a 44.1 kHz 16-bit stereo WAV.
fn write_sine_wav(path: &Path, freq: f64, seconds: f64) {
    let rate = 44_100_u32;
    let frames = (seconds * f64::from(rate)) as u32;
    let data_len = frames * 4;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data_len).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 4).to_le_bytes());
    bytes.extend(4_u16.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_len.to_le_bytes());
    for i in 0..frames {
        let phase = 2.0 * std::f64::consts::PI * freq * f64::from(i) / f64::from(rate);
        let sample = (phase.sin() * 9_000.0) as i16;
        bytes.extend(sample.to_le_bytes());
        bytes.extend(sample.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// Frequency of a pure tone in interleaved stereo, from its rising zero
/// crossings over the steady middle.
fn tone_frequency(out: &[f32], rate: u32) -> f64 {
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let part = &left[left.len() / 4..left.len() * 3 / 4];
    let rising: Vec<f64> = part
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + f64::from(-w[0] / (w[1] - w[0])))
        .collect();
    (rising.len() - 1) as f64 * f64::from(rate) / (rising[rising.len() - 1] - rising[0])
}

/// The note lands in tune, the bass as well as the rest, whatever the speed, and
/// the length is what the speed says. The stretcher's own transposition is out
/// by up to 12 cents at 110 Hz, which is why it does not do this.
#[test]
fn a_transposition_is_in_tune_and_keeps_the_length_of_the_speed() {
    let dir = tempfile::tempdir().unwrap();
    for freq in [110.0_f64, 440.0, 2_000.0] {
        let path = dir.path().join(format!("{freq}.wav"));
        write_sine_wav(&path, freq, 3.0);
        for (speed, keep_pitch, semitones) in [
            (1.0_f32, true, 3.0_f32),
            (1.0, true, -5.0),
            (1.0, true, 7.0),
            (1.25, true, 2.0),
            (1.25, false, 2.0),
        ] {
            let mut stream = open_tone(&path);
            stream.set_time_shift(shift(speed, keep_pitch, semitones));
            let out = decode_to_end(&mut stream, 44_100);
            assert!(stream.stretcher.is_some());
            assert_eq!(stream.output_delay_frames(), 0);
            let expected = (3.0 * 44_100.0 / f64::from(speed)).round() as usize;
            // Two stages that each round to the frame.
            assert!(
                (out.len() / 2).abs_diff(expected) <= 1,
                "{speed}× {semitones} st: {} for {expected}",
                out.len() / 2
            );
            let want = freq
                * 2f64.powf(f64::from(semitones) / 12.0)
                * if keep_pitch { 1.0 } else { f64::from(speed) };
            let cents = 1200.0 * (tone_frequency(&out, 44_100) / want).log2();
            assert!(
                cents.abs() < 1.0,
                "{freq} Hz {speed}× {semitones} st: {cents} cents"
            );
        }
    }
}

/// A seek drops what the stretcher held and starts over with no delay: the rest
/// of the track comes out at its length, once.
#[test]
fn a_seek_starts_the_stretcher_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    let mut stream = open_tone(&path);
    stream.set_time_shift(shift(1.25, true, 0.0));
    let out = decode_to_end(&mut stream, 44_100);
    assert_eq!(out.len() / 2, 35_280);

    stream.seek_ms(0);
    stream.rebuild_resampler(1.25, 44_100, 2).unwrap();
    stream.reset_decoder();
    assert!(stream.stretcher.is_none());
    assert_eq!(decode_to_end(&mut stream, 44_100).len() / 2, 35_280);

    let landed = stream.seek_ms(500);
    stream.reset_decoder();
    assert!(stream.stretcher.is_some());
    let left = decode_to_end(&mut stream, 44_100).len() / 2;
    let expected = (1_000 - landed) as f64 * 44.1 / 1.25;
    assert!(
        (left as f64 - expected).abs() < 40.0,
        "{left} for {expected}"
    );
}

/// Back at 1× mid-track, the stretcher is gone and the rest is the source's.
#[test]
fn leaving_the_shift_mid_track_drops_the_stretcher() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tone.wav");
    write_wav(&path);
    let mut stream = open_tone(&path);
    stream.set_time_shift(shift(1.5, true, 0.0));
    let (mut out, mut scratch) = (Vec::new(), Vec::new());
    for _ in 0..4 {
        stream
            .decode_next(&mut out, &mut scratch, 44_100, &stereo())
            .unwrap();
    }
    assert!(stream.stretcher.is_some());
    stream.set_time_shift(shift(1.0, true, 0.0));
    stream.rebuild_resampler(1.0, 44_100, 2).unwrap();
    stream.seek_ms(0);
    stream.reset_decoder();
    assert!(stream.stretcher.is_none());
    assert_eq!(
        decode_to_end(&mut stream, 44_100),
        decode_to_end(&mut open_tone(&path), 44_100)
    );
}
