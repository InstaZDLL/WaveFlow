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
/// so the library never displays tracks that would error at play time.
///
/// What is absent, and why — three different reasons that are worth not
/// confusing, because only one of them is a licence question:
///
/// - **Opus** is missing for a purely technical reason: symphonia ships
///   no Opus decoder. Not a licence issue in any sense — Opus is an IETF
///   standard (RFC 6716), royalty-free by design, and `libopus` is BSD.
///   Playing it means an out-of-tree decoder, the way DSD is handled
///   below. The gap that leaves — `.ogg` is accepted for Vorbis, and an
///   extension cannot tell Vorbis from Opus inside the container — is
///   why this list is no longer the whole filter: [`is_scannable_audio`]
///   reads the stream of an ambiguous container before believing it.
/// - **WMA** is genuinely proprietary: Microsoft, unpublished
///   specification, patent-encumbered. This one stays out.
/// - **AIFF** is neither. It is a container, not a codec — Apple's
///   answer to WAV, holding the same PCM — and symphonia reads it from
///   `symphonia-format-riff`, the crate `wav` already pulls in.
///
/// `.aifc` is deliberately not listed. symphonia's AIFC support covers
/// the PCM-shaped compression types (`none`, `sowt`, `twos`, `fl32`,
/// `alaw`, …) and refuses the rest, so accepting the extension would
/// index files that cannot play, on a format nobody writes any more.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aiff", "aif", "ogg", "oga", "m4a", "mp4", "aac",
    // DSD: handled by the in-tree audio::dsd pipeline (symphonia
    // doesn't decode DSD), with metadata read via audio::dsd::metadata.
    "dsf", "dff",
];

/// Containers whose extension names the box and not the stream inside
/// it. Everything else in [`AUDIO_EXTENSIONS`] implies its codec closely
/// enough that the extension is the whole answer; these do not, so they
/// pay for one header read before the scanner believes them.
const AMBIGUOUS_CONTAINERS: &[&str] = &["ogg", "oga"];

/// Why the local engine cannot decode this container's stream, or `None`
/// when it can.
///
/// Stated as a deny list rather than an allow list on purpose. An allow
/// list that forgot a codec would drop files the engine plays perfectly
/// well — including anything lofty reports as `Custom`, and any codec a
/// future symphonia feature adds. Every entry here is a stream lofty
/// identifies and this build ships no decoder for.
pub fn undecodable_stream(file_type: &FileType) -> Option<&'static str> {
    match file_type {
        FileType::Opus => Some("Opus"),
        FileType::Speex => Some("Speex"),
        _ => None,
    }
}

/// Whether the scanner should index this path at all.
///
/// The extension check alone was wrong for one family of files: `.ogg`
/// and `.oga` name a container, and Opus travels in the same one Vorbis
/// does. Such a file passed a list built for Vorbis, was indexed, and
/// then failed at play time — the single thing [`AUDIO_EXTENSIONS`]
/// exists to prevent. So an ambiguous container is opened far enough to
/// read which stream it actually carries.
///
/// A file that stops qualifying is not deleted from a library that
/// already holds it: it simply stops being walked, and the scan's
/// disappearance sweep marks the row unavailable, keeping its likes,
/// playlists and play history. Ship a decoder for it later and the next
/// scan brings the same row back.
pub fn is_scannable_audio(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    let extension = extension.to_lowercase();
    if !AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        return false;
    }
    if !AMBIGUOUS_CONTAINERS.contains(&extension.as_str()) {
        return true;
    }
    // A container we cannot read is left to the extractor to report:
    // refusing it here would turn an unreadable file into an absent one.
    let Some(file_type) = lofty::probe::Probe::open(path)
        .ok()
        .and_then(|probe| probe.guess_file_type().ok())
        .and_then(|probe| probe.file_type())
    else {
        return true;
    };
    match undecodable_stream(&file_type) {
        Some(codec) => {
            tracing::info!(
                path = %path.display(),
                codec,
                "container holds a stream this build cannot decode; not indexing it"
            );
            false
        }
        None => true,
    }
}

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
    /// ReplayGain the file already carries, normalised to the
    /// ReplayGain 2.0 scale. Refreshed on every scan because an
    /// external tagger can add or recompute it at any time; playback
    /// prefers it over our own analysis, which is why it is read here
    /// rather than only when the user asks for an analysis pass.
    pub replay_gain: super::replay_gain::ReplayGainTags,
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
    let out_path = artwork_dir.join(format!("{}.{}", hash, format));
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

    let out_path = artwork_dir.join(format!("{}.{}", hash, format));
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
        let path = entry.path();
        // Fast path: `read_dir`'s file_type avoids a `stat` for regular
        // files. But it does NOT follow symlinks (a symlinked image
        // reports `is_symlink()`, not `is_file()`), so fall back to the
        // link-following `Path::is_file` for those — preserving the
        // pre-cache behaviour for symlinked sidecars while still paying
        // the extra syscall only on the rare symlink.
        let is_file = match entry.file_type() {
            Ok(t) if t.is_file() => true,
            Ok(t) if t.is_symlink() => path.is_file(),
            Ok(_) => false,
            Err(_) => path.is_file(),
        };
        if !is_file {
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

    let out_path = artwork_dir.join(format!("{}.{}", hash, format));
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

    /// Wrap one packet in an Ogg beginning-of-stream page.
    ///
    /// Enough of a page for the codec magic that follows the header to be
    /// found where a reader looks for it, which is all identifying the
    /// stream needs. The CRC is left zero: nothing on this path verifies
    /// it, and a real encoder is a heavy price for one header.
    fn ogg_page(packet: &[u8]) -> Vec<u8> {
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.push(0); // stream structure version
        page.push(0x02); // beginning of stream
        page.extend_from_slice(&0u64.to_le_bytes()); // granule position
        page.extend_from_slice(&1u32.to_le_bytes()); // bitstream serial
        page.extend_from_slice(&0u32.to_le_bytes()); // page sequence
        page.extend_from_slice(&0u32.to_le_bytes()); // checksum
        page.push(1); // one segment
        page.push(packet.len() as u8);
        page.extend_from_slice(packet);
        page
    }

    fn opus_identification() -> Vec<u8> {
        let mut packet = b"OpusHead".to_vec();
        packet.push(1); // version
        packet.push(2); // channels
        packet.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
        packet.extend_from_slice(&48_000u32.to_le_bytes()); // input rate
        packet.extend_from_slice(&0i16.to_le_bytes()); // output gain
        packet.push(0); // channel mapping family
        packet
    }

    fn vorbis_identification() -> Vec<u8> {
        let mut packet = vec![1];
        packet.extend_from_slice(b"vorbis");
        packet.extend_from_slice(&0u32.to_le_bytes()); // version
        packet.push(2); // channels
        packet.extend_from_slice(&44_100u32.to_le_bytes()); // sample rate
        packet.extend_from_slice(&[0u8; 12]); // the three bitrate hints
        packet.push(0xB8); // block sizes
        packet.push(1); // framing
        packet
    }

    /// The bug this guards: `.ogg` names a container, not a codec, so an
    /// Opus stream reached a list built for Vorbis, was indexed, and then
    /// failed the moment it was played.
    #[test]
    fn an_ogg_is_judged_by_its_stream_and_not_by_its_extension() {
        let dir = tempfile::tempdir().expect("tempdir");

        let opus = dir.path().join("opus-in-disguise.ogg");
        write_bytes(&opus, &ogg_page(&opus_identification()));
        assert!(
            !is_scannable_audio(&opus),
            "an Opus stream in an .ogg container is not indexed"
        );

        let vorbis = dir.path().join("really-vorbis.ogg");
        write_bytes(&vorbis, &ogg_page(&vorbis_identification()));
        assert!(
            is_scannable_audio(&vorbis),
            "the container the extension was built for still passes"
        );

        // An unambiguous extension is not opened at all, so a file the
        // extractor will refuse later still reaches it — an unreadable
        // file has to be reported, not made to disappear.
        let broken = dir.path().join("truncated.flac");
        write_bytes(&broken, b"not really audio");
        assert!(is_scannable_audio(&broken));

        // And an ambiguous container nothing can identify is left to the
        // extractor for the same reason.
        let unreadable = dir.path().join("empty.oga");
        write_bytes(&unreadable, b"");
        assert!(is_scannable_audio(&unreadable));

        let cover = dir.path().join("cover.jpg");
        write_bytes(&cover, TINY_JPEG);
        assert!(!is_scannable_audio(&cover));
    }

    /// A minimal but real AIFF: `FORM`/`COMM`/`SSND`, 16-bit big-endian
    /// stereo PCM. Built here rather than committed as a fixture so the
    /// bytes the test relies on are visible next to the assertions.
    fn tiny_aiff(frames: u32) -> Vec<u8> {
        let channels: u16 = 2;
        let sample_size: u16 = 16;
        let data_len = frames as usize * channels as usize * 2;

        let mut comm = Vec::new();
        comm.extend_from_slice(&channels.to_be_bytes());
        comm.extend_from_slice(&frames.to_be_bytes());
        comm.extend_from_slice(&sample_size.to_be_bytes());
        // 44100 Hz as an IEEE 754 80-bit extended float: biased exponent
        // 16383 + 15, then 0xAC44 (44100) left-aligned in the significand,
        // whose leading integer bit is explicit in this format.
        comm.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);

        let mut ssnd = Vec::new();
        ssnd.extend_from_slice(&0u32.to_be_bytes()); // offset
        ssnd.extend_from_slice(&0u32.to_be_bytes()); // block size
        for frame in 0..frames {
            // A quiet ramp rather than silence: a decoder that returns the
            // right frame count of zeroes would pass on silence alone.
            let sample = (frame as i16).wrapping_mul(64);
            ssnd.extend_from_slice(&sample.to_be_bytes());
            ssnd.extend_from_slice(&sample.to_be_bytes());
        }

        let mut body = Vec::new();
        body.extend_from_slice(b"AIFF");
        body.extend_from_slice(b"COMM");
        body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
        body.extend_from_slice(&comm);
        body.extend_from_slice(b"SSND");
        body.extend_from_slice(&((8 + data_len) as u32).to_be_bytes());
        body.extend_from_slice(&ssnd);

        let mut out = Vec::new();
        out.extend_from_slice(b"FORM");
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// The `aiff` feature is what makes the extension list below honest:
    /// the scanner only offers what the engine can play, so this decodes a
    /// real file rather than asserting that a string is in a list.
    #[test]
    fn an_aiff_file_probes_and_decodes() {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::formats::probe::Hint;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tone.aiff");
        write_bytes(&path, &tiny_aiff(256));

        let file = fs::File::open(&path).expect("open");
        let mut hint = Hint::new();
        hint.with_extension("aiff");
        let mut format = symphonia::default::get_probe()
            .probe(
                &hint,
                MediaSourceStream::new(Box::new(file), Default::default()),
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("symphonia probes AIFF once the feature is on");

        let track = format
            .default_track(TrackType::Audio)
            .expect("the AIFF carries an audio track");
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .expect("audio codec params")
            .clone();
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .expect("AIFF holds the PCM `wav` already decodes");

        let mut decoded = 0u64;
        while let Ok(Some(packet)) = format.next_packet() {
            if packet.track_id != track_id {
                continue;
            }
            let buffer = decoder.decode(&packet).expect("decode");
            decoded += buffer.frames() as u64;
        }
        assert_eq!(decoded, 256, "every frame written comes back out");
    }

    /// The list states a policy, and the policy has edges that are easy to
    /// widen by accident.
    #[test]
    fn the_extension_list_admits_aiff_and_still_refuses_what_cannot_play() {
        for accepted in ["aiff", "aif", "wav", "flac", "mp3"] {
            assert!(
                AUDIO_EXTENSIONS.contains(&accepted),
                "{accepted} is playable and must be indexed"
            );
        }
        for refused in [
            // No symphonia decoder at all.
            "opus", // Proprietary, and staying out.
            "wma",  // AIFC's compressed forms are refused by the reader, so
            // accepting the extension would index files that cannot play.
            "aifc",
        ] {
            assert!(
                !AUDIO_EXTENSIONS.contains(&refused),
                "{refused} cannot be played and must not be indexed"
            );
        }
    }

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
