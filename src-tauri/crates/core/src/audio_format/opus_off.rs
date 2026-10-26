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
// Loaded through `#[path]`, this file resolves child modules from its own
// directory, where `tests.rs` is `audio_format`'s.
#[path = "opus_off/tests.rs"]
mod tests;
