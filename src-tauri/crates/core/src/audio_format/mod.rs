//! Audio file-format helpers that aren't tied to the desktop's
//! real-time `cpal` pipeline.
//!
//! For now: the in-tree DSD (Direct Stream Digital) parser, PCM
//! converter, and metadata reader. Symphonia doesn't decode DSD
//! natively (1-bit @ 2.8 / 5.6 / 11.2 / 22.5 MHz), so the scanner
//! ingests DSF / DFF tracks via this module and playback
//! (`playback::stream`) streams them through the same PCM converter.
//! The future `waveflow-server` will reuse the same code path when
//! transcoding DSD uploads.

pub mod aac;
pub mod ape;
pub mod dsd;
#[cfg(feature = "sqlite")]
pub mod format_filter;
// Opus decoding over libopus (#581). symphonia reads the containers
// but ships no Opus decoder, so this one is registered into a custom
// codec registry alongside the built-ins. Behind the `opus` feature
// (default on): without it the same `codecs()` answers, minus Opus.
#[cfg(feature = "opus")]
pub mod opus;
#[cfg(not(feature = "opus"))]
#[path = "opus_off.rs"]
pub mod opus;
pub mod wavpack;

use std::sync::OnceLock;

use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::formats::probe::Probe;

/// The format probe this app opens streams with: everything symphonia's
/// feature flags enable, plus the containers it has no reader for.
///
/// The container half of [`opus::codecs`], and the same rule applies: there
/// are three places that open a stream — playback, the analysis pass and the
/// scanner's probe — and a reader only one of them knows about is the
/// disagreement that puts an unplayable track in the library.
pub fn probe() -> &'static Probe {
    static PROBE: OnceLock<Probe> = OnceLock::new();
    PROBE.get_or_init(|| {
        let mut probe = Probe::default();
        symphonia::default::register_enabled_formats(&mut probe);
        probe.register_format::<wavpack::WavPackReader<'_>>();
        probe.register_format::<ape::ApeReader<'_>>();
        probe
    })
}

/// How long the file at `path` really plays, when it is a WavPack or Monkey's
/// Audio file cut short of the length its header announces. lofty reads that
/// header and repeats it. `None` for everything else, a whole file included.
pub fn held_duration_ms(path: &std::path::Path) -> Option<i64> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("wv") {
        wavpack::held_duration_ms(path)
    } else if extension.eq_ignore_ascii_case("ape") {
        ape::held_duration_ms(path)
    } else {
        None
    }
}

/// The decoders every build of [`opus::codecs`] carries on top of symphonia's.
fn register_own_decoders(registry: &mut CodecRegistry) {
    registry.register_audio_decoder::<wavpack::WavPackDecoder>();
    registry.register_audio_decoder::<ape::ApeDecoder>();
    // Same id as symphonia's AAC decoder and the same tier: this one
    // replaces it, and still hands it what it accepts.
    registry.register_audio_decoder::<aac::AacDecoder>();
}

/// A symphonia decoder's short name as the format chip spells codecs:
/// `mp3` → `MP3`, every `pcm_*` → `PCM`, `vorbis` → `Vorbis`. One spelling
/// for the scanner, which stores it on the track, and for playback, which
/// reports it for the file being played.
pub fn codec_label(short_name: &str) -> String {
    match short_name {
        n if n.starts_with("pcm") => "PCM".into(),
        "vorbis" => "Vorbis".into(),
        "opus" => "Opus".into(),
        "wavpack" => "WavPack".into(),
        n => n.to_ascii_uppercase(),
    }
}

#[cfg(test)]
mod tests;
