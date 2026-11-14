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

#[derive(Debug, Clone)]
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
///
/// lofty has no variant for WebP (what a video thumbnail usually is), so it
/// arrives as `Unknown("image/webp")`, and a picture can carry no MIME at all
/// or a wrong one: prefer [`cover_extension`], which reads the bytes.
pub fn extension_for_mime(mime: Option<&MimeType>) -> &'static str {
    match mime {
        Some(MimeType::Jpeg) => "jpg",
        Some(MimeType::Png) => "png",
        Some(MimeType::Gif) => "gif",
        Some(MimeType::Bmp) => "bmp",
        Some(MimeType::Tiff) => "tiff",
        Some(MimeType::Unknown(name)) if name.eq_ignore_ascii_case("image/webp") => "webp",
        _ => "bin",
    }
}

/// The extension a picture is stored under: what its bytes are when they
/// are a JPEG, a PNG or a WebP, else what its MIME type says. The tag's
/// word comes second because it is the weaker evidence: a downloader that
/// keeps a WebP thumbnail declares it `image/jpeg` as often as not, and the
/// store, the thumbnailer and the webview each take the extension for the
/// format.
pub fn cover_extension(mime: Option<&MimeType>, bytes: &[u8]) -> &'static str {
    crate::artwork::image_format::detect_image_format(bytes)
        .unwrap_or_else(|| extension_for_mime(mime))
}

/// Extract the front cover from the given tag, or its first non-empty picture
/// when none says it is one, hash-address it, and write
/// it to `<artwork_dir>/<hash>.<ext>` if missing. Returns the identifying
/// `ExtractedCover` or `None` when the tag has no picture with any bytes.
///
/// The write is idempotent: a file whose path already exists is assumed to
/// match (because blake3 hashes are content-addressed), so we never
/// overwrite on re-scan.
pub fn extract_cover(tag: &Tag, artwork_dir: &Path) -> Option<ExtractedCover> {
    // Not simply the first: a replaced cover lands behind the images a file
    // already holds, its back cover and booklet among them. An empty picture
    // is passed over rather than taken, in the search for the front cover and
    // in the fallback alike, or an empty front cover in first place would
    // hide the rest (#1071).
    let pictures = tag.pictures();
    let picture = pictures
        .iter()
        .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront && !p.data().is_empty())
        .or_else(|| pictures.iter().find(|p| !p.data().is_empty()))?;
    let bytes = picture.data();
    if bytes.is_empty() {
        return None;
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    let format = cover_extension(picture.mime_type(), bytes).to_string();
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
/// first match wins. Mirrors the convention most music software follows.
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

    // Single read_dir pass: cheaper than 6×7 = 42 `Path::exists` calls when the
    // directory is large.
    let files: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_string();
            Some((name, path))
        })
        .collect();
    let name = pick_folder_cover(files.iter().map(|(name, _)| name.as_str()))?;
    let picked = files.iter().find(|(file, _)| file == name)?.1.clone();

    let bytes = fs::read(&picked).ok()?;
    store_folder_cover(
        &bytes,
        picked.extension().and_then(|s| s.to_str()),
        artwork_dir,
    )
}

/// The sidecar cover among the file names of one directory, in the priority
/// order above: lowercased `(stem, ext)` looked up against both lists. A share
/// calls this too, since its listing already holds the names and asking for a
/// directory again costs a round trip.
pub fn pick_folder_cover<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut candidates: HashMap<(String, String), &'a str> = HashMap::new();
    for name in names {
        let path = Path::new(name);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        if let (Some(s), Some(e)) = (stem, ext) {
            candidates.insert((s, e), name);
        }
    }
    FOLDER_COVER_STEMS
        .iter()
        .flat_map(|stem| {
            FOLDER_COVER_EXTENSIONS
                .iter()
                .map(move |ext| (stem.to_string(), ext.to_string()))
        })
        .find_map(|key| candidates.get(&key).copied())
}

/// Hash-address a sidecar cover's bytes into the shared artwork dir.
/// `extension` is the file's own; `None` stores it as `jpg`.
pub fn store_folder_cover(
    bytes: &[u8],
    extension: Option<&str>,
    artwork_dir: &Path,
) -> Option<ExtractedCover> {
    if bytes.is_empty() {
        return None;
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    let format = extension
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
    if let Err(err) = crate::atomic_write::store_content_addressed(&out_path, bytes) {
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
