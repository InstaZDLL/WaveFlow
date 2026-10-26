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

pub mod dsd;
// Opus decoding over libopus (#581). symphonia reads the containers
// but ships no Opus decoder, so this one is registered into a custom
// codec registry alongside the built-ins. Behind the `opus` feature
// (default on): without it the same `codecs()` answers, minus Opus.
#[cfg(feature = "opus")]
pub mod opus;
#[cfg(not(feature = "opus"))]
#[path = "opus_off.rs"]
pub mod opus;

/// A symphonia decoder's short name as the format chip spells codecs:
/// `mp3` → `MP3`, every `pcm_*` → `PCM`, `vorbis` → `Vorbis`. One spelling
/// for the scanner, which stores it on the track, and for playback, which
/// reports it for the file being played.
pub fn codec_label(short_name: &str) -> String {
    match short_name {
        n if n.starts_with("pcm") => "PCM".into(),
        "vorbis" => "Vorbis".into(),
        "opus" => "Opus".into(),
        n => n.to_ascii_uppercase(),
    }
}

#[cfg(test)]
mod tests;
