use symphonia::core::codecs::audio::well_known::{CODEC_ID_FLAC, CODEC_ID_OPUS};

use super::codecs;

/// The build without libopus still decodes what symphonia does, and
/// does not claim Opus: a registry that did would hand the scanner's
/// probe a decoder this build never compiled.
#[test]
fn decodes_symphonias_codecs_and_not_opus() {
    assert!(codecs().get_audio_decoder(CODEC_ID_FLAC).is_some());
    assert!(codecs().get_audio_decoder(CODEC_ID_OPUS).is_none());
}
