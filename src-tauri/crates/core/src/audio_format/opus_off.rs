//! The codec registry of a build without the `opus` feature.
//!
//! Same function, same registry, minus the libopus decoder: the three
//! places that open a stream keep calling [`codecs`] and simply cannot
//! decode Opus. That is a build only a consumer that decodes elsewhere
//! asks for — `waveflow-server` hands every stream to FFmpeg and uses
//! this crate for tags, so compiling libopus (and needing cmake to do
//! it) bought it nothing. The desktop keeps the feature through the
//! default set.
//!
//! [`crate::scanner::AUDIO_EXTENSIONS`] still admits Opus here, on
//! purpose: the list answers which files belong in a library, and for
//! the server, whose decoder is FFmpeg, an Opus file does.

use std::sync::OnceLock;

use symphonia::core::codecs::registry::CodecRegistry;

/// Everything symphonia's feature flags enable, and nothing more.
pub fn codecs() -> &'static CodecRegistry {
    static REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry
    })
}

#[cfg(test)]
mod tests {
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
}
