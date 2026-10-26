use super::*;

#[test]
fn a_media_playlist_numbers_its_segments() {
    let text =
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:2847871\n\
                #EXTINF:3.968,\nseg-2847871.ts\n#EXTINF:4.011,\nseg-2847872.ts\n";
    let Playlist::Media(list) = parse(text).unwrap() else {
        panic!("a media playlist");
    };
    assert_eq!(list.target_duration_s, 4.0);
    assert!(!list.ended);
    assert_eq!(list.segments.len(), 2);
    assert_eq!(list.segments[1].sequence, 2_847_872);
    assert_eq!(list.segments[1].uri, "seg-2847872.ts");
    assert!((list.segments[0].duration_s - 3.968).abs() < 1e-9);
}

#[test]
fn a_multivariant_playlist_picks_its_best_audio_variant() {
    let text = "#EXTM3U\n\
        #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.5\"\nlow/index.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"\nhigh/index.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=900000,CODECS=\"avc1.4d401f,mp4a.40.2\"\nvideo/index.m3u8\n";
    let Playlist::Multivariant(list) = parse(text).unwrap() else {
        panic!("a multivariant playlist");
    };
    assert_eq!(list.variants.len(), 3);
    assert_eq!(list.pick(), Some("high/index.m3u8"));
}

#[test]
fn a_video_only_ladder_takes_its_lightest_variant_and_an_audio_rendition_wins() {
    let text = "#EXTM3U\n\
        #EXT-X-STREAM-INF:BANDWIDTH=900000,CODECS=\"avc1.4d401f,mp4a.40.2\"\nhd.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=300000,CODECS=\"avc1.4d400d,mp4a.40.2\"\nsd.m3u8\n";
    let Playlist::Multivariant(list) = parse(text).unwrap() else {
        panic!();
    };
    assert_eq!(list.pick(), Some("sd.m3u8"));
    let text = format!(
        "{text}#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Main\",DEFAULT=YES,URI=\"audio.m3u8\"\n"
    );
    let Playlist::Multivariant(list) = parse(&text).unwrap() else {
        panic!();
    };
    assert_eq!(list.pick(), Some("audio.m3u8"));
}

#[test]
fn what_cannot_be_played_is_refused() {
    assert_eq!(
        parse("#EXTM3U\nhttp://a/stream.mp3\n"),
        Err(HlsError::NotHls)
    );
    assert_eq!(
        parse("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:6,\na.ts\n"),
        Err(HlsError::Encrypted)
    );
    assert_eq!(
        parse("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6,\na.m4s\n"),
        Err(HlsError::FragmentedMp4)
    );
    assert_eq!(
        parse("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n#EXT-X-BYTERANGE:1000@0\nall.aac\n"),
        Err(HlsError::ByteRange)
    );
    assert!(matches!(
        parse("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:6,\na.ts\n"),
        Ok(Playlist::Media(_))
    ));
}

#[test]
fn durations_that_are_not_one_fall_back_to_the_target() {
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:inf\n#EXTINF:NaN,\na.ts\n#EXTINF:1e300,\nb.ts\n#EXTINF:-4,\nc.ts\n";
    let Playlist::Media(list) = parse(text).unwrap() else {
        panic!();
    };
    assert_eq!(list.target_duration_s, 6.0);
    assert!(list.segments.iter().all(|s| s.duration_s == 6.0));
}

#[test]
fn attributes_quoted_or_not() {
    let list = "BANDWIDTH=128000,CODECS=\"mp4a.40.2,mp4a.40.5\",RESOLUTION=1x1";
    assert_eq!(attribute(list, "BANDWIDTH"), Some("128000"));
    assert_eq!(attribute(list, "codecs"), Some("mp4a.40.2,mp4a.40.5"));
    assert_eq!(attribute(list, "RESOLUTION"), Some("1x1"));
    assert_eq!(attribute(list, "MISSING"), None);
}
