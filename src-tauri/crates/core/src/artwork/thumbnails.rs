//! Thumbnail pipeline for hash-addressed cover art.
//!
//! For every full-size artwork on disk we maintain two pre-resized JPEG
//! variants alongside the original:
//!
//! ```text
//! <hash>.<ext>      // full
//! <hash>_1x.jpg     // 64×64 (list rows)
//! <hash>_2x.jpg     // 128×128 (grid tiles)
//! ```
//!
//! Generation is offloaded to a worker thread via [`spawn_thumbnail_job`]
//! so the scan and Deezer download paths stay non-blocking.

use std::path::{Path, PathBuf};

use anyhow::Result;
use fast_image_resize as fr;
use fr::{ResizeOptions, Resizer};
use image::ImageReader;

pub const THUMB_SMALL: u32 = 64;
pub const THUMB_MEDIUM: u32 = 128;

pub fn thumbnail_path(base_dir: &Path, hash: &str, size: u32) -> PathBuf {
    let suffix = match size {
        THUMB_SMALL => "_1x",
        THUMB_MEDIUM => "_2x",
        _ => "",
    };
    base_dir.join(format!("{hash}{suffix}.jpg"))
}

pub fn generate_thumbnails(source_path: &Path, base_dir: &Path, hash: &str) -> Result<()> {
    let img = ImageReader::open(source_path)?
        .with_guessed_format()?
        .decode()?;
    let rgba = img.to_rgba8();
    let src_w = rgba.width();
    let src_h = rgba.height();
    let src_image =
        fr::images::Image::from_vec_u8(src_w, src_h, rgba.into_raw(), fr::PixelType::U8x4)?;

    let mut resizer = Resizer::new();
    for &target in &[THUMB_SMALL, THUMB_MEDIUM] {
        let out = thumbnail_path(base_dir, hash, target);
        if out.exists() {
            continue;
        }
        let mut dst_image = fr::images::Image::new(target, target, fr::PixelType::U8x4);
        resizer.resize(&src_image, &mut dst_image, &ResizeOptions::new())?;
        let buf: Vec<u8> = dst_image.into_vec();
        let rgba_img = image::RgbaImage::from_raw(target, target, buf)
            .ok_or_else(|| anyhow::anyhow!("rgba_img build failed"))?;
        let dyn_img = image::DynamicImage::ImageRgba8(rgba_img);
        let rgb = dyn_img.to_rgb8();
        // Encoded in memory and stored through a partial, so a write cut
        // short never sits under the final name, where the `exists()`
        // check above would keep it for good.
        let mut jpeg = std::io::Cursor::new(Vec::new());
        rgb.write_to(&mut jpeg, image::ImageFormat::Jpeg)?;
        crate::atomic_write::store_content_addressed(&out, jpeg.get_ref())?;
    }
    Ok(())
}

pub fn spawn_thumbnail_job(source_path: PathBuf, base_dir: PathBuf, hash: String) {
    std::thread::spawn(move || {
        if let Err(e) = generate_thumbnails(&source_path, &base_dir, &hash) {
            tracing::warn!(error = %e, %hash, "thumbnail generation failed");
        }
    });
}

/// Return `(picture_path_1x, picture_path_2x)` as absolute strings, or
/// `None` for any variant that does not exist on disk yet. The caller
/// is expected to also surface the full-size path separately.
pub fn thumbnail_paths_for(base_dir: &Path, hash: &str) -> (Option<String>, Option<String>) {
    let p1 = thumbnail_path(base_dir, hash, THUMB_SMALL);
    let p2 = thumbnail_path(base_dir, hash, THUMB_MEDIUM);
    let s1 = if p1.exists() {
        Some(p1.to_string_lossy().into_owned())
    } else {
        None
    };
    let s2 = if p2.exists() {
        Some(p2.to_string_lossy().into_owned())
    } else {
        None
    };
    (s1, s2)
}

/// Below this many lookups a batch asks the disk per file, as
/// [`thumbnail_paths_for`] does; at or above it, it lists the directory
/// once. A listing reads every entry of the directory -- three per
/// cover -- so it only pays off once the batch is large enough, and the
/// lists it is meant for (a library's albums, its tracks) are hundreds
/// to tens of thousands of rows.
pub const LIST_THRESHOLD: usize = 64;

/// Which thumbnails exist, for a whole batch of hashes in one directory.
///
/// [`thumbnail_paths_for`] asks the filesystem twice per hash. Each of
/// those is a file open on Windows, measured at 130 to 400 µs under the
/// stock antivirus: the album list of a 20 000-track library spent about
/// ten times longer on them than on its query, and the track list, two
/// per track, five to fifteen seconds. Listing the directory once answers every
/// row of the batch from memory instead.
///
/// The answers are the same: a thumbnail is written whole under its
/// final name (`store_content_addressed`), so an entry with that name is
/// a complete file, and a partial one carries a suffix no lookup asks
/// for. A thumbnail written after the listing reads as missing, which
/// the caller already handles -- it is the state of a cover whose
/// thumbnails the worker has not produced yet.
pub struct ThumbnailIndex {
    dir: PathBuf,
    /// The directory's file names, or `None` to ask the disk per lookup:
    /// a small batch, or a directory that could not be listed.
    names: Option<std::collections::HashSet<String>>,
}

impl ThumbnailIndex {
    /// An index over `dir` for about `lookups` hashes. A directory that
    /// does not exist yet holds no thumbnail, so it lists as empty; one
    /// that cannot be read for another reason falls back to asking per
    /// file rather than calling every cover missing.
    pub fn new(dir: &Path, lookups: usize) -> Self {
        let names = if lookups < LIST_THRESHOLD {
            None
        } else {
            // An error part-way through the listing would leave it short,
            // and a short listing calls present thumbnails missing: any
            // error, at the open or on an entry, falls back to probing.
            let listed = std::fs::read_dir(dir).and_then(|entries| {
                entries
                    .map(|entry| entry.map(|e| e.file_name().into_string().ok()))
                    .filter_map(Result::transpose)
                    .collect::<std::io::Result<std::collections::HashSet<String>>>()
            });
            match listed {
                Ok(names) => Some(names),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Some(std::collections::HashSet::new())
                }
                Err(e) => {
                    tracing::warn!(error = %e, "thumbnail directory not listable; probing per file");
                    None
                }
            }
        };
        Self {
            dir: dir.to_path_buf(),
            names,
        }
    }

    /// The directory the index answers for.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether `path`, a file directly inside the indexed directory,
    /// exists: `Path::exists`, answered from the listing when there is one.
    pub fn holds(&self, path: &Path) -> bool {
        match &self.names {
            Some(names) => {
                path.parent() == Some(self.dir.as_path())
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| names.contains(name))
            }
            None => path.exists(),
        }
    }

    /// [`thumbnail_paths_for`], answered from the listing when there is one.
    pub fn paths_for(&self, hash: &str) -> (Option<String>, Option<String>) {
        let lookup = |size| {
            let path = thumbnail_path(&self.dir, hash, size);
            self.holds(&path)
                .then(|| path.to_string_lossy().into_owned())
        };
        (lookup(THUMB_SMALL), lookup(THUMB_MEDIUM))
    }
}

#[cfg(test)]
mod tests;
