//! Pure-Rust file extractors used by the scanner: hash, cover, artist
//! image, rating, musical key, tag-to-struct mapping.
//!
//! Everything here is filesystem + lofty + image; no SQL, no Tauri.
//! The orchestrator (`scan_folder_inner` in `crates/app`) calls these
//! helpers per file and then hands the resulting [`ExtractedFile`] to
//! the [`super::upserts`] family for the DB writes.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use lofty::file::FileType;
use lofty::picture::MimeType;
use lofty::tag::{ItemKey, Tag, TagType};

use super::canonical::canonical_name;

/// Extensions considered "audio files" by the scanner. Limited to
/// formats the symphonia + cpal engine can actually decode and play,
/// so the library never displays tracks that would error at play
/// time. Opus / WMA / AIFF are intentionally absent — symphonia
/// doesn't ship a mainline decoder for Opus, WMA is Microsoft
/// proprietary, and AIFF isn't in the default feature set.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "m4a", "mp4", "aac",
    // DSD: handled by the in-tree audio::dsd pipeline (symphonia
    // doesn't decode DSD), with metadata read via audio::dsd::metadata.
    "dsf", "dff",
];

/// Bytes hashed from each of the file's head and tail in the partial
/// path. 1 MiB each — large enough that distinct tracks differ inside
/// the window (leading frames) and that tag rewrites land in it (ID3v2
/// at the head, ID3v1 / APE / Lyrics3 at the tail), small enough that a
/// multi-MB track reads ~2 MiB instead of its whole length.
const HASH_CHUNK_BYTES: u64 = 1024 * 1024;

/// Content hash used for dedup + tag-edit detection.
///
/// Files larger than `2 * HASH_CHUNK_BYTES` are hashed over their size +
/// first chunk + last chunk only, instead of every byte. For a typical
/// audio library this cuts the scan's disk I/O several-fold (full-file
/// hashing was the dominant cost — reading ~9 GB to scan 900 tracks)
/// while staying a strong identity:
/// - moved / renamed files keep the same bytes → same hash (dedup holds),
/// - a tag rewrite shifts bytes in the head/tail window → hash changes,
///   so the scanner still re-extracts edited files,
/// - the file length is folded in, so two files sharing head+tail but
///   differing in size never collide.
///
/// Blind spot: two *distinct* files with identical size, head and tail
/// but different middle bytes would collide. For real music that does
/// not occur. Smaller files (≤ `2 * HASH_CHUNK_BYTES`) are hashed whole.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&len.to_le_bytes());

    if len <= 2 * HASH_CHUNK_BYTES {
        // Small enough to read fully — also the path most callers and
        // unit tests exercise.
        let mut reader = std::io::BufReader::new(file);
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    } else {
        let chunk = HASH_CHUNK_BYTES as usize;
        let mut head = vec![0u8; chunk];
        file.read_exact(&mut head)?;
        hasher.update(&head);

        let mut tail = vec![0u8; chunk];
        file.seek(SeekFrom::End(-(HASH_CHUNK_BYTES as i64)))?;
        file.read_exact(&mut tail)?;
        hasher.update(&tail);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

/// Full-content BLAKE3 hash — every byte. Slower than [`hash_file`]
/// (which reads only the head + tail of large files), so it's NOT used
/// on the hot scan path. The duplicate-detection flow calls it to
/// confirm that tracks sharing the cheap partial digest are *really*
/// byte-identical before offering to delete one — closing the partial
/// hash's middle-byte blind spot for that destructive path.
pub fn hash_file_full(path: &Path) -> std::io::Result<String> {
    let file = fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Everything the scanner reads off disk for a single audio file. Populated
/// inside `spawn_blocking` so the tokio reactor never stalls on I/O.
pub struct ExtractedFile {
    pub abs_path: String,
    pub size: i64,
    pub modified_ms: i64,
    pub hash: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Raw Album Artist text from the source tag (`TPE2` / `aART` /
    /// `ALBUMARTIST` / `Album Artist`). Used as the album-grouping
    /// authority — when present, two tracks share an album even if
    /// their per-track Artist tags differ (featurings, lead-vocal
    /// rotations on K-pop EPs, etc.).
    pub album_artist: Option<String>,
    /// `TCMP` (ID3v2) / `cpil` (MP4) / `COMPILATION` (Vorbis / APE)
    /// flag. When `true` the scanner uses a synthetic "Various
    /// Artists" album artist so a true compilation merges its tracks
    /// under a single album row even when no Album Artist tag exists.
    pub is_compilation: bool,
    pub genre: Option<String>,
    pub year: Option<i64>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_ms: i64,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    /// Bits per sample (16 for CD-quality, 24/32 for Hi-Res masters).
    /// Lossy codecs (MP3, AAC) typically don't expose this — left as
    /// `None` so the UI's Hi-Res badge logic can short-circuit without
    /// inspecting the codec separately.
    pub bit_depth: Option<i64>,
    /// Short codec / container label inferred from the file type
    /// (e.g. `"FLAC"`, `"MP3"`, `"AAC"`, `"WAV"`). Drives the format
    /// chip on the player footer.
    pub codec: Option<String>,
    /// Tagged musical key when the file carries one (`TKEY` / ID3v2
    /// or `INITIALKEY` / Vorbis-MP4-APE). Whatever notation the
    /// upstream tagger chose stays as-is — could be `Am`, `F#`, or
    /// the Camelot wheel `8A`.
    pub musical_key: Option<String>,
    /// Embedded cover art extracted and hash-addressed during the scan. Only
    /// the first picture is kept (lofty exposes them in order and the first
    /// is usually the `CoverFront`). `None` when the tag has no pictures.
    pub cover_art: Option<ExtractedCover>,
    /// Raw POPM byte (0-255) for ID3v2 files, or a normalised value
    /// derived from the `RATING` text field for Vorbis/FLAC/MP4. `None`
    /// when neither tag carries a rating.
    pub rating: Option<u8>,
}

pub struct ExtractedCover {
    /// Hex-encoded blake3 hash of the picture bytes — used as the filename
    /// stem so identical artwork embedded in 20 tracks of an album yields a
    /// single file on disk.
    pub hash: String,
    /// File extension matching the picture's MIME type (jpg/png/webp/...).
    pub format: String,
    /// Provenance label written to `artwork.source`. Either `"embedded"`
    /// (lifted from the tag) or `"folder"` (sidecar cover.jpg / folder.png
    /// / front.webp etc. next to the audio file).
    pub source: &'static str,
}

/// Map lofty's `FileType` enum to a short uppercase label suitable
/// for the UI's format chip. Falls back to `None` when lofty can't
/// determine a recognized container — we'd rather hide the chip
/// than print "Unknown".
pub fn file_type_label(ft: FileType) -> Option<String> {
    match ft {
        FileType::Mpeg => Some("MP3".into()),
        FileType::Flac => Some("FLAC".into()),
        FileType::Mp4 => Some("AAC".into()),
        FileType::Aac => Some("AAC".into()),
        FileType::Wav => Some("WAV".into()),
        FileType::Vorbis => Some("Vorbis".into()),
        FileType::Opus => Some("Opus".into()),
        FileType::Aiff => Some("AIFF".into()),
        FileType::Speex => Some("Speex".into()),
        FileType::Ape => Some("APE".into()),
        FileType::WavPack => Some("WavPack".into()),
        FileType::Custom(name) => Some(name.to_string()),
        _ => None,
    }
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
    let out_path = artwork_dir.join(format!("{}.{}", &hash, &format));
    if !out_path.exists() {
        if let Err(err) = fs::write(&out_path, bytes) {
            tracing::warn!(path = %out_path.display(), error = %err, "failed to write artwork");
            return None;
        }
    }
    crate::artwork::thumbnails::spawn_thumbnail_job(
        out_path,
        artwork_dir.to_path_buf(),
        hash.clone(),
    );
    Some(ExtractedCover {
        hash,
        format,
        source: "embedded",
    })
}

/// Canonical filename stems searched for in the track's parent directory
/// when the audio file carries no embedded picture. Order matters — the
/// first match wins. Mirrors the convention used by foobar2000, MusicBee,
/// Plex, Kodi, RustMusic.
const FOLDER_COVER_STEMS: &[&str] = &["cover", "folder", "front", "albumart", "album", "artwork"];

/// File extensions accepted as folder cover candidates. Limited to formats
/// the `image` crate decodes via the features enabled in `Cargo.toml`, so
/// every match downstream of this fn is guaranteed to be readable by the
/// thumbnail pipeline.
const FOLDER_COVER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif", "tiff"];

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

    let out_path = artwork_dir.join(format!("{}.{}", &hash, &format));
    if !out_path.exists() {
        if let Err(err) = fs::write(&out_path, &bytes) {
            tracing::warn!(path = %out_path.display(), error = %err, "failed to write folder cover");
            return None;
        }
    }
    crate::artwork::thumbnails::spawn_thumbnail_job(
        out_path,
        artwork_dir.to_path_buf(),
        hash.clone(),
    );
    Some(ExtractedCover {
        hash,
        format,
        source: "folder",
    })
}

/// Stems recognised as a sidecar artist photo at any ancestor level of a
/// track. Matched verbatim (lowercased); a stem-aware match against the
/// artist's canonical name handles the `<artist>.jpg` convention.
const ARTIST_IMAGE_STEMS: &[&str] = &["artist", "performer", "band"];

/// Maximum number of parent directories walked upward from the track to
/// find an artist photo. Covers the two common layouts called out in
/// issue #31:
///   1. `<root>/<artist>/<album>/track.flac` → 2 levels up (`<artist>/`).
///   2. `<root>/<album>/track.flac`         → 1 level up (`<album>/`),
///      and even the album folder itself can hold an `<artist>.jpg`.
///
/// 3 covers the occasional `<root>/<artist>/<album>/CD1/track.flac` rip.
const ARTIST_IMAGE_MAX_DEPTH: usize = 3;

/// Look for a sidecar artist image next to the track. Walks up to
/// `ARTIST_IMAGE_MAX_DEPTH` parent directories from `track_path` and
/// accepts the first match where either:
///   - the file stem is in [`ARTIST_IMAGE_STEMS`] (`artist.jpg`,
///     `performer.png`, …), or
///   - the file stem's canonical form equals `artist_canonical` (covers
///     `Daft Punk.jpg` sitting at the root of a `Daft Punk/` folder).
///
/// Hash-addressed write into `artwork_dir` like every other cover so a
/// later GC can dedup across artists and albums.
pub fn extract_artist_image(
    track_path: &Path,
    artist_canonical: &str,
    artwork_dir: &Path,
) -> Option<ExtractedCover> {
    // One-shot callers (VA linking, the rescan command) pay a throwaway
    // cache; the per-scan hot path uses `extract_artist_image_cached`
    // with a shared one.
    let mut cache = ArtistImageDirCache::new();
    extract_artist_image_cached(track_path, artist_canonical, artwork_dir, &mut cache)
}

/// An image file in a directory, pre-parsed for artist matching. Built
/// once per directory by [`read_dir_artist_images`] and cached so the
/// `fs::read_dir` + per-entry work isn't repeated for every artist /
/// track that walks through the same folder.
#[derive(Clone)]
pub struct DirImageCandidate {
    /// `canonical_name(stem)` — matched against an artist's canonical
    /// name for the `<Artist>.jpg` convention.
    canon_stem: String,
    /// Lowercased stem — matched against [`ARTIST_IMAGE_STEMS`]
    /// (`artist` / `performer` / `band`).
    stem_lower: String,
    path: PathBuf,
}

/// Per-scan memo of each directory's image candidates. Keyed on the
/// directory path; the sidecar-artist-image walk reuses it across every
/// artist and track that shares an ancestor folder — the `read_dir`
/// (+ a `file_type` per entry + a `canonical_name` per image) is the
/// dominant first-scan cost, and it's identical for a given directory
/// regardless of which artist is being resolved.
pub type ArtistImageDirCache = HashMap<PathBuf, Vec<DirImageCandidate>>;

/// Read a directory's image-file candidates once. Artist-independent.
/// Uses `entry.file_type()` (populated by `read_dir` on Windows/Linux)
/// instead of `Path::is_file` so there's no extra `stat` per entry.
fn read_dir_artist_images(dir: &Path) -> Vec<DirImageCandidate> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        let (Some(stem), Some(ext)) = (stem, ext) else {
            continue;
        };
        if !FOLDER_COVER_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        out.push(DirImageCandidate {
            canon_stem: canonical_name(&stem),
            stem_lower: stem,
            path,
        });
    }
    out
}

/// Match cached directory candidates against one artist. Mirrors the
/// old `find_artist_image_in_dir` precedence: an exact `<Artist>.jpg`
/// (canonical) match wins over a generic `artist`/`performer`/`band`
/// stem, and among generic stems the earliest in [`ARTIST_IMAGE_STEMS`]
/// wins.
fn match_artist_image(candidates: &[DirImageCandidate], artist_canonical: &str) -> Option<PathBuf> {
    let mut named_match: Option<&PathBuf> = None;
    let mut stem_match: Option<(usize, &PathBuf)> = None;
    for c in candidates {
        if c.canon_stem == artist_canonical {
            named_match.get_or_insert(&c.path);
            continue;
        }
        if let Some(rank) = ARTIST_IMAGE_STEMS.iter().position(|s| *s == c.stem_lower) {
            match &stem_match {
                Some((current_rank, _)) if *current_rank <= rank => {}
                _ => stem_match = Some((rank, &c.path)),
            }
        }
    }
    named_match.or(stem_match.map(|(_, p)| p)).cloned()
}

/// Cache-backed variant of [`extract_artist_image`]. Walks the same up
/// to [`ARTIST_IMAGE_MAX_DEPTH`] ancestor dirs, but each directory's
/// candidate list is read once via the shared `cache` and reused for
/// every artist / track that passes through it.
pub fn extract_artist_image_cached(
    track_path: &Path,
    artist_canonical: &str,
    artwork_dir: &Path,
    cache: &mut ArtistImageDirCache,
) -> Option<ExtractedCover> {
    if artist_canonical.is_empty() {
        return None;
    }

    let mut current = track_path.parent();
    for _ in 0..ARTIST_IMAGE_MAX_DEPTH {
        let Some(dir) = current else { break };
        let candidates = cache
            .entry(dir.to_path_buf())
            .or_insert_with(|| read_dir_artist_images(dir));
        if let Some(found) = match_artist_image(candidates, artist_canonical) {
            return write_artist_image(&found, artwork_dir);
        }
        current = dir.parent();
    }
    None
}

pub fn find_artist_image_in_dir(dir: &Path, artist_canonical: &str) -> Option<PathBuf> {
    match_artist_image(&read_dir_artist_images(dir), artist_canonical)
}

pub fn write_artist_image(picked: &Path, artwork_dir: &Path) -> Option<ExtractedCover> {
    let bytes = fs::read(picked).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let format = picked
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "jpg".to_string());
    let format = if format == "jpeg" {
        "jpg".to_string()
    } else {
        format
    };

    let out_path = artwork_dir.join(format!("{}.{}", &hash, &format));
    if !out_path.exists() {
        if let Err(err) = fs::write(&out_path, &bytes) {
            tracing::warn!(
                path = %out_path.display(),
                error = %err,
                "failed to write artist image",
            );
            return None;
        }
    }
    crate::artwork::thumbnails::spawn_thumbnail_job(
        out_path,
        artwork_dir.to_path_buf(),
        hash.clone(),
    );
    Some(ExtractedCover {
        hash,
        format,
        source: "folder",
    })
}

/// Extract a 0-255 rating from a tag. POPM frames (ID3v2) are stored by
/// lofty as raw `ItemValue::Binary` under `ItemKey::Popularimeter`: the
/// frame body is `<email>\0<rating:u8><counter:u32+>`, so the rating is
/// the byte right after the first NUL terminator. Vorbis/FLAC/MP4 expose
/// `RATING` as plain text 0-100 which we rescale to 0-255.
pub fn extract_rating(tag: &Tag) -> Option<u8> {
    if matches!(tag.tag_type(), TagType::Id3v2) {
        if let Some(bytes) = tag.get_binary(ItemKey::Popularimeter, false) {
            let nul_pos = bytes.iter().position(|b| *b == 0)?;
            return bytes.get(nul_pos + 1).copied();
        }
    }
    if let Some(text) = tag.get_string(ItemKey::Popularimeter) {
        let trimmed = text.trim();
        if let Ok(val) = trimmed.parse::<u16>() {
            let clamped = val.min(100);
            return Some((clamped * 255 / 100) as u8);
        }
    }
    None
}

/// Read the tagged musical key, if any. ID3v2 stores it as `TKEY`,
/// Vorbis comments / MP4 / APE / WavPack as `INITIALKEY` — lofty
/// unifies both behind `ItemKey::InitialKey`. Empty strings are
/// coalesced to `None` so the UI's "—" placeholder kicks in
/// instead of a blank cell.
pub fn extract_musical_key(tag: &Tag) -> Option<String> {
    let raw = tag.get_string(ItemKey::InitialKey)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Pull the Album Artist tag and trim it. Lofty's `ItemKey::AlbumArtist`
/// already abstracts the per-container mapping (`TPE2` / `aART` /
/// `ALBUMARTIST` / `Album Artist`). Empty / whitespace-only strings are
/// treated as missing so the grouping code falls back to the per-track
/// Artist exactly like before.
pub fn extract_album_artist(tag: &Tag) -> Option<String> {
    let raw = tag.get_string(ItemKey::AlbumArtist)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read the compilation flag (`TCMP` / `cpil` / `COMPILATION` / `Compilation`).
/// Lofty stores the value as a stringified `0` / `1` regardless of the
/// underlying container; anything that parses to a non-zero integer or the
/// literal `true` is treated as "this is a compilation".
pub fn extract_compilation_flag(tag: &Tag) -> bool {
    let Some(raw) = tag.get_string(ItemKey::FlagCompilation) else {
        return false;
    };
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        return true;
    }
    matches!(trimmed.parse::<i64>(), Ok(n) if n != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::canonical_name;

    fn write_bytes(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write fixture");
    }

    /// Smallest valid 1x1 JPEG — enough to satisfy the non-empty check
    /// and exercise the hash + write + spawn_thumbnail_job pipeline
    /// without dragging the `image` crate into the unit test.
    const TINY_JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0xFF, 0xD9,
    ];

    #[test]
    fn folder_cover_picks_priority_stem_over_alphabetical_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let folder = dir.path().join("album");
        fs::create_dir_all(&folder).unwrap();

        // `albumart` ranks below `cover` in FOLDER_COVER_STEMS; even though
        // it sorts first alphabetically, the priority list must win.
        write_bytes(&folder.join("albumart.jpg"), TINY_JPEG);
        write_bytes(&folder.join("cover.png"), TINY_JPEG);

        let track = folder.join("01.flac");
        write_bytes(&track, b"not really audio");

        let cover = extract_folder_cover(&track, &artwork_dir).expect("cover found");
        assert_eq!(
            cover.format, "png",
            "cover.png should win over albumart.jpg"
        );
        assert_eq!(cover.source, "folder");
    }

    #[test]
    fn folder_cover_normalises_jpeg_extension() {
        let dir = tempfile::tempdir().unwrap();
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let folder = dir.path().join("album");
        fs::create_dir_all(&folder).unwrap();

        write_bytes(&folder.join("front.JPEG"), TINY_JPEG);
        let track = folder.join("01.flac");
        write_bytes(&track, b"x");

        let cover = extract_folder_cover(&track, &artwork_dir).expect("cover found");
        // `jpeg` must collapse to `jpg` so the artwork dir has one
        // canonical extension per MIME.
        assert_eq!(cover.format, "jpg");
    }

    #[test]
    fn folder_cover_returns_none_when_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let folder = dir.path().join("album");
        fs::create_dir_all(&folder).unwrap();

        // Recognised extension but stem isn't in the priority list.
        write_bytes(&folder.join("scan-of-booklet.jpg"), TINY_JPEG);
        let track = folder.join("01.flac");
        write_bytes(&track, b"x");

        assert!(extract_folder_cover(&track, &artwork_dir).is_none());
    }

    #[test]
    fn artist_image_finds_stem_in_parent_folder() {
        // Layout: <root>/<Artist>/<Album>/<track>
        let dir = tempfile::tempdir().unwrap();
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let artist_dir = dir.path().join("Daft Punk");
        let album_dir = artist_dir.join("Discovery");
        fs::create_dir_all(&album_dir).unwrap();

        write_bytes(&artist_dir.join("artist.jpg"), TINY_JPEG);
        let track = album_dir.join("01.flac");
        write_bytes(&track, b"x");

        let cover = extract_artist_image(&track, &canonical_name("Daft Punk"), &artwork_dir)
            .expect("artist image found two levels up");
        assert_eq!(cover.source, "folder");
        assert_eq!(cover.format, "jpg");
    }

    #[test]
    fn artist_image_matches_canonical_name_stem() {
        // Layout: <root>/<Album>/<track> with `<Artist>.jpg` beside the album.
        let dir = tempfile::tempdir().unwrap();
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let album_dir = dir.path().join("Discovery");
        fs::create_dir_all(&album_dir).unwrap();

        write_bytes(&album_dir.join("Daft Punk.png"), TINY_JPEG);
        let track = album_dir.join("01.flac");
        write_bytes(&track, b"x");

        let cover = extract_artist_image(&track, &canonical_name("daft punk"), &artwork_dir)
            .expect("canonical-name stem match");
        assert_eq!(cover.format, "png");
    }

    #[test]
    fn artist_image_ignores_unrelated_named_image() {
        let dir = tempfile::tempdir().unwrap();
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let album_dir = dir.path().join("Discovery");
        fs::create_dir_all(&album_dir).unwrap();

        // `cover.jpg` is an album cover, not an artist photo.
        write_bytes(&album_dir.join("cover.jpg"), TINY_JPEG);
        let track = album_dir.join("01.flac");
        write_bytes(&track, b"x");

        assert!(
            extract_artist_image(&track, &canonical_name("Daft Punk"), &artwork_dir).is_none(),
            "should not pick up album cover as artist image",
        );
    }

    #[test]
    fn artist_image_returns_none_for_empty_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let folder = dir.path().join("album");
        fs::create_dir_all(&folder).unwrap();
        write_bytes(&folder.join("artist.jpg"), TINY_JPEG);
        let track = folder.join("01.flac");
        write_bytes(&track, b"x");

        // Empty canonical → defensive bail-out so we don't match every dir.
        assert!(extract_artist_image(&track, "", &artwork_dir).is_none());
    }

    #[test]
    fn folder_cover_writes_hash_addressed_file() {
        let dir = tempfile::tempdir().unwrap();
        let artwork_dir = dir.path().join("artwork");
        fs::create_dir_all(&artwork_dir).unwrap();
        let folder = dir.path().join("album");
        fs::create_dir_all(&folder).unwrap();

        write_bytes(&folder.join("cover.jpg"), TINY_JPEG);
        let track = folder.join("01.flac");
        write_bytes(&track, b"x");

        let cover = extract_folder_cover(&track, &artwork_dir).expect("cover");
        let on_disk = artwork_dir.join(format!("{}.{}", cover.hash, cover.format));
        assert!(on_disk.exists(), "hash-addressed file must be written");
    }

    #[test]
    fn hash_file_small_is_content_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        write_bytes(&a, b"hello world");
        write_bytes(&b, b"hello world");
        assert_eq!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
        write_bytes(&b, b"hello WORLD");
        assert_ne!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
    }

    #[test]
    fn hash_file_large_detects_head_and_size_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let mut data = vec![7u8; 3 * 1024 * 1024];
        write_bytes(&path, &data);
        let base = hash_file(&path).unwrap();

        // A change inside the head window flips the hash.
        data[10] = 42;
        write_bytes(&path, &data);
        assert_ne!(base, hash_file(&path).unwrap());

        // A change inside the tail window flips the hash too (guards the
        // seek-to-end + read_exact of the tail chunk).
        data[10] = 7; // restore head
        let last = data.len() - 10;
        data[last] = 42;
        write_bytes(&path, &data);
        assert_ne!(base, hash_file(&path).unwrap());

        // A size change flips the hash even with otherwise-identical
        // head + tail (the length is folded into the digest).
        data[last] = 7; // restore tail
        data.push(7); // grow by one byte
        write_bytes(&path, &data);
        assert_ne!(base, hash_file(&path).unwrap());
    }

    #[test]
    fn hash_file_large_blind_to_middle() {
        // Documents the partial-hash tradeoff: a byte strictly between
        // the head and tail windows doesn't change the digest. Distinct
        // real tracks never hit this — their head and/or size differ.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let mut data = vec![7u8; 3 * 1024 * 1024];
        write_bytes(&path, &data);
        let base = hash_file(&path).unwrap();

        data[1_500_000] = 99; // > 1 MiB (head end), < 2 MiB (tail start)
        write_bytes(&path, &data);
        assert_eq!(base, hash_file(&path).unwrap());
    }
}
