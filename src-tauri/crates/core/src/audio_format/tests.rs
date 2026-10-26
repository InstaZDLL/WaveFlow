use super::codec_label;

#[test]
fn codecs_are_spelled_as_the_format_chip_spells_them() {
    assert_eq!(codec_label("mp3"), "MP3");
    assert_eq!(codec_label("flac"), "FLAC");
    assert_eq!(codec_label("pcm_s16le"), "PCM");
    assert_eq!(codec_label("vorbis"), "Vorbis");
    assert_eq!(codec_label("opus"), "Opus");
}
