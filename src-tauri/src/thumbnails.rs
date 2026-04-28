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
    let src_image = fr::images::Image::from_vec_u8(
        src_w,
        src_h,
        rgba.into_raw(),
        fr::PixelType::U8x4,
    )?;

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
        rgb.save_with_format(&out, image::ImageFormat::Jpeg)?;
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
