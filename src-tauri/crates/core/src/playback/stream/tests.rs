use super::*;

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
        .decode_next(&mut out, &mut scratch, 48_000, 2)
        .unwrap()
    {}
    // Asked again at the end, it adds nothing more.
    let at_end = out.len();
    assert!(stream
        .decode_next(&mut out, &mut scratch, 48_000, 2)
        .unwrap());
    assert_eq!(out.len(), at_end);

    let delay = match &stream.resampler {
        Resampler::Fft { inner, .. } => inner.output_delay(),
        Resampler::Passthrough => panic!("44.1 kHz to 48 kHz resamples"),
    };
    assert_eq!(out.len() / 2, delay + 48_000);
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
            .decode_next(&mut out, &mut scratch, 44_100, 2)
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
