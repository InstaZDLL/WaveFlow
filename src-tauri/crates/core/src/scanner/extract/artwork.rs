//! Album artwork: the cover embedded in a tag and the folder cover beside
//! a track, written to the artwork store under their content hash.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use lofty::picture::MimeType;
use lofty::tag::Tag;

/// Where an `artwork` row came from.
///
/// The variants are exactly the values `artwork.source`'s `CHECK`
/// constraint accepts (initial profile migration), and nothing else.
/// That constraint is only enforced when a row is written, so a free
/// `&str` let a caller pass a value the database refuses — the artist
/// banner picker wrote `"theaudiodb"` and failed on every suggestion
/// (#750) while every test stayed green. Typed, a new source is a
/// compile error until the constraint grows the matching value.
///
/// The constraint cannot simply be widened: `artwork` is the parent of
/// several foreign keys, and SQLite only changes a `CHECK` by rebuilding
/// the table, which `foreign_keys = ON` turns into a cascading delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtworkSource {
    /// Lifted from the audio file's own tag.
    Embedded,
    /// A sidecar image next to the audio file (`cover.jpg`, `folder.png`…).
    Folder,
    /// Fetched from Deezer.
    Deezer,
    /// Chosen by the user — a file they picked, or a suggestion from any
    /// provider they picked it from.
    Manual,
}

impl ArtworkSource {
    /// The value stored in `artwork.source` (and mirrored into
    /// `album.artwork_source`, which has no constraint of its own).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Folder => "folder",
            Self::Deezer => "deezer",
            Self::Manual => "manual",
        }
    }
}

pub struct ExtractedCover {
    /// Hex-encoded blake3 hash of the picture bytes — used as the filename
    /// stem so identical artwork embedded in 20 tracks of an album yields a
    /// single file on disk.
    pub hash: String,
    /// File extension matching the picture's MIME type (jpg/png/webp/...).
    pub format: String,
    /// Provenance written to `artwork.source`: [`ArtworkSource::Embedded`]
    /// (lifted from the tag) or [`ArtworkSource::Folder`] (sidecar
    /// cover.jpg / folder.png / front.webp etc. next to the audio file).
    pub source: ArtworkSource,
}

/// Pick a reasonable filename extension for lofty's MIME type enum. Unknown
/// / exotic formats fall through to `"bin"` so the file is still written and
/// the UI can decide what to do with it.
pub fn extension_for_mime(mime: Option<&MimeType>) -> &'static str {
    match mime {
        Some(MimeType::Jpeg) => "jpg",
        Some(MimeType::Png) => "png",
        Some(MimeType::Gif) => "gif",
        Some(MimeType::Bmp) => "bmp",
        Some(MimeType::Tiff) => "tiff",
        _ => "bin",
    }
}

/// Extract the first picture from the given tag, hash-address it, and write
/// it to `<artwork_dir>/<hash>.<ext>` if missing. Returns the identifying
/// `ExtractedCover` or `None` when the tag has no pictures.
///
/// The write is idempotent: a file whose path already exists is assumed to
/// match (because blake3 hashes are content-addressed), so we never
/// overwrite on re-scan.
pub fn extract_cover(tag: &Tag, artwork_dir: &Path) -> Option<ExtractedCover> {
    let picture = tag.pictures().first()?;
    let bytes = picture.data();
    if bytes.is_empty() {
        return None;
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    let format = extension_for_mime(picture.mime_type()).to_string();
    let out_path = artwork_dir.join(format!("{}.{}", hash, format));
    if let Err(err) = crate::atomic_write::store_content_addressed(&out_path, bytes) {
        tracing::warn!(path = %out_path.display(), error = %err, "failed to write artwork");
        return None;
    }
    crate::artwork::thumbnails::spawn_thumbnail_job(
        out_path,
        artwork_dir.to_path_buf(),
        hash.clone(),
    );
    Some(ExtractedCover {
        hash,
        format,
        source: ArtworkSource::Embedded,
    })
}

/// Canonical filename stems searched for in the track's parent directory
/// when the audio file carries no embedded picture. Order matters — the
/// first match wins. Mirrors the convention used by foobar2000, MusicBee,
/// Plex and Kodi.
const FOLDER_COVER_STEMS: &[&str] = &["cover", "folder", "front", "albumart", "album", "artwork"];

/// File extensions accepted as folder cover candidates. Limited to formats
/// the `image` crate decodes via the features enabled in `Cargo.toml`, so
/// every match downstream of this fn is guaranteed to be readable by the
/// thumbnail pipeline.
pub const FOLDER_COVER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif", "tiff"];

/// Look for a sidecar cover image (cover.jpg / folder.png / front.webp / ...)
/// next to the track. Returns an `ExtractedCover` written to the shared
/// artwork dir, hash-addressed like embedded pictures.
///
/// Used as a fallback when the audio file has no embedded picture — common
/// for FLAC/WAV libraries ripped from CD where the artwork sits beside the
/// tracks rather than inside them.
pub fn extract_folder_cover(track_path: &Path, artwork_dir: &Path) -> Option<ExtractedCover> {
    let parent = track_path.parent()?;
    let entries = fs::read_dir(parent).ok()?;

    // Index siblings by lowercased (stem, ext) for O(1) lookup against the
    // priority lists above. Single read_dir pass — cheaper than 6×7 = 42
    // `Path::exists` calls when the directory is large.
    let mut candidates: HashMap<(String, String), PathBuf> = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        if let (Some(s), Some(e)) = (stem, ext) {
            candidates.insert((s, e), path);
        }
    }

    let picked = FOLDER_COVER_STEMS
        .iter()
        .flat_map(|stem| {
            FOLDER_COVER_EXTENSIONS
                .iter()
                .map(move |ext| (stem.to_string(), ext.to_string()))
        })
        .find_map(|key| candidates.get(&key).cloned())?;

    let bytes = fs::read(&picked).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let format = picked
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "jpg".to_string());
    // Normalise `jpeg` to `jpg` so the artwork dir doesn't end up with two
    // entries pointing at the same MIME.
    let format = if format == "jpeg" {
        "jpg".to_string()
    } else {
        format
    };

    let out_path = artwork_dir.join(format!("{}.{}", hash, format));
    if let Err(err) = crate::atomic_write::store_content_addressed(&out_path, &bytes) {
        tracing::warn!(path = %out_path.display(), error = %err, "failed to write folder cover");
        return None;
    }
    crate::artwork::thumbnails::spawn_thumbnail_job(
        out_path,
        artwork_dir.to_path_buf(),
        hash.clone(),
    );
    Some(ExtractedCover {
        hash,
        format,
        source: ArtworkSource::Folder,
    })
}

#[cfg(test)]
mod tests;
