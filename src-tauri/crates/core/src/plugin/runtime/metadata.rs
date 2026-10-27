//! Calls into a `waveflow:metadata/enricher` plugin, in each of the
//! world's three versions.

use wasmtime::Store;

use crate::plugin::PluginPaths;

use super::{HostCtx, PluginRuntime, SourceError};

// ----- metadata-v1 invocation helpers -------------------------------------
//
// Instantiates + calls the `waveflow:metadata/enricher` exports. Phase 3
// wires only `album-info` (the motion-artwork path needs it); `artist-info`
// + `lyrics` get helpers when a plugin consumes them. Reuses [`SourceError`]
// — its Instantiate / Trap / Plugin variants are world-agnostic.

/// Owned mirror of `waveflow:metadata/enricher/album-info` (1.1.0).
#[derive(Debug, Clone, Default)]
pub struct AlbumInfo {
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub track_count: Option<u32>,
    /// Looping video URL for the square animated cover — a directly-
    /// playable progressive `.mp4` (rendered in a native `<video>`, no
    /// HLS.js; an HLS source is resolved to an mp4 rendition first).
    pub motion_cover_url: Option<String>,
    /// Taller lock-screen variant; `None` falls back to the square.
    pub motion_cover_tall_url: Option<String>,
}

define_instantiate!(
    instantiate_metadata,
    crate::plugin::bindings::metadata::Plugin
);

/// Call the guest's `album-info(artist, title)`.
pub fn metadata_album_info(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    artist: &str,
    title: &str,
) -> Result<AlbumInfo, SourceError> {
    let (mut store, plugin) = instantiate_metadata(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_metadata_enricher()
        .call_album_info(&mut store, artist, title)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(info) => Ok(AlbumInfo {
            description: info.description,
            cover_url: info.cover_url,
            track_count: info.track_count,
            motion_cover_url: info.motion_cover_url,
            motion_cover_tall_url: info.motion_cover_tall_url,
        }),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

// ----- metadata-v2 invocation helpers -------------------------------------
//
// The v2 world (issue #585) adds the `lyrics` call v1 declared and never
// wired. Owned mirrors of its records so the command layer never depends on
// wasmtime; the app crate maps these onto its own `LyricsFormat` and
// validates each document with `detect_format` before anything is cached.

/// Owned mirror of `waveflow:metadata/enricher/lyrics-format` (2.0.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsDocFormat {
    Plain,
    Lrc,
    EnhancedLrc,
    Ttml,
}

/// Owned mirror of `associated-kind` (2.0.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssociatedKind {
    Translation,
    Pronunciation,
}

/// Owned mirror of `lyrics-document` (2.0.0).
///
/// `content` is whatever the provider served, untouched — TTML carries
/// per-word timing, singer attribution and background vocals that no
/// model on this side represents yet, and re-serialising would drop them.
#[derive(Debug, Clone)]
pub struct LyricsDocument {
    pub content: String,
    pub format: LyricsDocFormat,
    pub language: Option<String>,
}

/// Owned mirror of `associated-document` (2.0.0).
#[derive(Debug, Clone)]
pub struct AssociatedDocument {
    pub kind: AssociatedKind,
    pub document: LyricsDocument,
}

/// Owned mirror of `lyrics-bundle` (2.0.0) — one fetch result, cached and
/// replaced as a unit so a primary document is never left paired with
/// another provider's translation.
#[derive(Debug, Clone)]
pub struct LyricsBundle {
    pub primary: LyricsDocument,
    pub associated: Vec<AssociatedDocument>,
}

define_instantiate!(
    instantiate_metadata_v2,
    crate::plugin::bindings::metadata_v2::Plugin
);

/// Call a v2 guest's `album-info(artist, title)`.
///
/// v2 is a superset of v1, so it exports this too. Without a v2 path the
/// motion-artwork surface would enumerate v1 plugins only, and the first
/// plugin to migrate — `apple-artwork`, which is the reason the world
/// exists — would stop serving animated covers with nothing to say why.
pub fn metadata_v2_album_info(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    artist: &str,
    title: &str,
) -> Result<AlbumInfo, SourceError> {
    let (mut store, plugin) = instantiate_metadata_v2(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_metadata_enricher()
        .call_album_info(&mut store, artist, title)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(info) => Ok(AlbumInfo {
            description: info.description,
            cover_url: info.cover_url,
            track_count: info.track_count,
            motion_cover_url: info.motion_cover_url,
            motion_cover_tall_url: info.motion_cover_tall_url,
        }),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

/// Call the guest's `lyrics(artist, title)`.
///
/// `Ok(None)` is the provider saying it has nothing for this track — a
/// real negative the caller may remember. `Err` means the lookup could
/// not be made, and nothing should be written.
pub fn metadata_v2_lyrics(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    artist: &str,
    title: &str,
) -> Result<Option<LyricsBundle>, SourceError> {
    use crate::plugin::bindings::metadata_v2::exports::waveflow::metadata::enricher as guest;

    fn map_format(f: guest::LyricsFormat) -> LyricsDocFormat {
        match f {
            guest::LyricsFormat::Plain => LyricsDocFormat::Plain,
            guest::LyricsFormat::Lrc => LyricsDocFormat::Lrc,
            guest::LyricsFormat::EnhancedLrc => LyricsDocFormat::EnhancedLrc,
            guest::LyricsFormat::Ttml => LyricsDocFormat::Ttml,
        }
    }

    fn map_doc(d: guest::LyricsDocument) -> LyricsDocument {
        LyricsDocument {
            content: d.content,
            format: map_format(d.format),
            language: d.language,
        }
    }

    let (mut store, plugin) = instantiate_metadata_v2(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_metadata_enricher()
        .call_lyrics(&mut store, artist, title)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(None) => Ok(None),
        Ok(Some(bundle)) => Ok(Some(LyricsBundle {
            primary: map_doc(bundle.primary),
            associated: bundle
                .associated
                .into_iter()
                .map(|a| AssociatedDocument {
                    kind: match a.kind {
                        guest::AssociatedKind::Translation => AssociatedKind::Translation,
                        guest::AssociatedKind::Pronunciation => AssociatedKind::Pronunciation,
                    },
                    document: map_doc(a.document),
                })
                .collect(),
        })),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

// ----- metadata-v3 invocation helpers -------------------------------------
//
// v3 asks `lyrics` about a track rather than two strings. Its records are
// otherwise v2's, so the answers map onto the same owned mirrors above.

/// Owned mirror of `track-query` (3.0.0): what the host knows about the
/// track lyrics are asked for. Everything past `title` is what the file
/// says, and `None` when it says nothing.
#[derive(Debug, Clone, Default)]
pub struct TrackQuery {
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub duration_ms: Option<u32>,
    pub isrc: Option<String>,
}

define_instantiate!(
    instantiate_metadata_v3,
    crate::plugin::bindings::metadata_v3::Plugin
);

/// Call a v3 guest's `album-info(artist, title)` — unchanged from v2, and
/// asked for the same reason: animated covers come from any metadata world.
pub fn metadata_v3_album_info(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    artist: &str,
    title: &str,
) -> Result<AlbumInfo, SourceError> {
    let (mut store, plugin) = instantiate_metadata_v3(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_metadata_enricher()
        .call_album_info(&mut store, artist, title)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(info) => Ok(AlbumInfo {
            description: info.description,
            cover_url: info.cover_url,
            track_count: info.track_count,
            motion_cover_url: info.motion_cover_url,
            motion_cover_tall_url: info.motion_cover_tall_url,
        }),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

/// Call a v3 guest's `lyrics(track)`. Same contract as
/// [`metadata_v2_lyrics`]: `Ok(None)` is a real "nothing for this track",
/// `Err` means the lookup could not be made.
pub fn metadata_v3_lyrics(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    track: &TrackQuery,
) -> Result<Option<LyricsBundle>, SourceError> {
    use crate::plugin::bindings::metadata_v3::exports::waveflow::metadata::enricher as guest;

    fn map_format(f: guest::LyricsFormat) -> LyricsDocFormat {
        match f {
            guest::LyricsFormat::Plain => LyricsDocFormat::Plain,
            guest::LyricsFormat::Lrc => LyricsDocFormat::Lrc,
            guest::LyricsFormat::EnhancedLrc => LyricsDocFormat::EnhancedLrc,
            guest::LyricsFormat::Ttml => LyricsDocFormat::Ttml,
        }
    }

    fn map_doc(d: guest::LyricsDocument) -> LyricsDocument {
        LyricsDocument {
            content: d.content,
            format: map_format(d.format),
            language: d.language,
        }
    }

    let query = guest::TrackQuery {
        artist: track.artist.clone(),
        title: track.title.clone(),
        album: track.album.clone(),
        duration_ms: track.duration_ms,
        isrc: track.isrc.clone(),
    };
    let (mut store, plugin) = instantiate_metadata_v3(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_metadata_enricher()
        .call_lyrics(&mut store, &query)
        .map_err(|e| SourceError::Trap(format!("{e:#}")))?;
    match result {
        Ok(None) => Ok(None),
        Ok(Some(bundle)) => Ok(Some(LyricsBundle {
            primary: map_doc(bundle.primary),
            associated: bundle
                .associated
                .into_iter()
                .map(|a| AssociatedDocument {
                    kind: match a.kind {
                        guest::AssociatedKind::Translation => AssociatedKind::Translation,
                        guest::AssociatedKind::Pronunciation => AssociatedKind::Pronunciation,
                    },
                    document: map_doc(a.document),
                })
                .collect(),
        })),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}
