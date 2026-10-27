//! Sidecar artist images: the picture named after an artist, or `artist.jpg`,
//! in a track's folder or one of its parents.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::scanner::canonical::canonical_name;

use super::{ArtworkSource, ExtractedCover, FOLDER_COVER_EXTENSIONS};

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
    if let Err(err) = crate::atomic_write::store_content_addressed(&out_path, &bytes) {
        tracing::warn!(
            path = %out_path.display(),
            error = %err,
            "failed to write artist image",
        );
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
