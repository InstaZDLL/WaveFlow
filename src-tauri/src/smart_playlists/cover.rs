//! Composite cover generator for smart playlists.
//!
//! Given up to N artist images (already cached locally as JPEG by
//! [`crate::metadata_artwork`]), produce a 640×640 cover that slices each
//! image into a vertical strip, centre-crops each strip to fill the canvas,
//! and applies a subtle bottom gradient so the React-rendered "Daily Mix N"
//! label stays readable on top.
//!
//! Output is written into the same `metadata_artwork/<hash>.jpg` shared cache
//! so a regenerated cover dedupes against an unchanged input set — no
//! orphaned files between runs unless the cluster's top artists actually
//! change.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use fast_image_resize::images::Image as FirImage;
use fast_image_resize::{PixelType, ResizeOptions, Resizer};
use image::{ImageBuffer, ImageFormat, Rgb, RgbImage};

use crate::error::{AppError, AppResult};
use crate::metadata_artwork;

/// Final canvas side in pixels. Spotify uses 640×640 for playlist covers and
/// it's a comfortable retina size — anything larger inflates JPEG bytes
/// without buying noticeable quality at the sidebar / carousel scale where
/// the cover is consumed.
const CANVAS_PX: u32 = 640;

/// JPEG quality for the encoded cover. 85 is the standard "visually
/// lossless for photographic content" setpoint and keeps each cover under
/// ~80 KB.
const JPEG_QUALITY: u8 = 85;

/// Hard cap on the number of strips we composite. More than 3 starts to look
/// like a contact sheet rather than a curated mix.
const MAX_STRIPS: usize = 3;

/// Maximum tiles in the 2×2 grid layout. Anything beyond is ignored so the
/// callers can pass their full ordered list without manual truncation.
const MAX_GRID_TILES: usize = 4;

/// Build a composite cover from the supplied image paths and write it to the
/// shared `metadata_artwork` cache. Returns the blake3 hash of the encoded
/// JPEG, ready to store in `playlist.cover_hash`.
///
/// Layout is picked from the input count so callers don't have to choose:
/// - 1 image → fills the whole canvas
/// - 2 images → vertical halves
/// - 3 images → 3 vertical strips (the Daily Mix look)
/// - 4+ images → 2×2 grid (Spotify-style auto-playlist cover)
///
/// Images that fail to decode are skipped silently; the function only errors
/// if the *final* JPEG can't be produced (zero usable inputs, encoder error,
/// disk write).
pub fn build_composite_cover(image_paths: &[PathBuf], metadata_dir: &Path) -> AppResult<String> {
    let take = MAX_GRID_TILES.max(MAX_STRIPS);
    // Dedupe by path so a Daily Mix whose top 3 artists share a picture
    // (or whose fallback album arts all point at the same release)
    // collapses to a single full-canvas tile instead of a contact-sheet
    // of identical thumbnails. Mirrors the hash-level dedup that
    // `playlist_cover::top_track_artwork_paths` already applies for
    // user playlists — paths here are hash-keyed too (metadata_artwork
    // cache + per-profile artwork dir), so path equality is hash equality.
    let mut seen: std::collections::HashSet<&PathBuf> =
        std::collections::HashSet::with_capacity(image_paths.len());
    let tiles: Vec<RgbImage> = image_paths
        .iter()
        .filter(|p| seen.insert(*p))
        .take(take)
        .filter_map(|p| match image::open(p) {
            Ok(img) => Some(img.to_rgb8()),
            Err(err) => {
                tracing::warn!(path = %p.display(), ?err, "smart cover: skip undecodable input");
                None
            }
        })
        .collect();

    if tiles.is_empty() {
        return Err(AppError::Audio(
            "smart cover: no decodable input images".into(),
        ));
    }

    let canvas = if tiles.len() >= 4 {
        composite_grid_2x2(&tiles[..4])?
    } else {
        composite_strips(&tiles)?
    };
    let bytes = encode_jpeg(&canvas)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let out = metadata_artwork::path_for_hash(metadata_dir, &hash);
    if !out.exists() {
        std::fs::write(&out, &bytes)
            .map_err(|e| AppError::Audio(format!("smart cover write: {e}")))?;
    }
    Ok(hash)
}

/// Backwards-compatible shim — Daily Mix specifically prefers strips for 1-3
/// inputs (matches Spotify's Daily Mix visual). New callers should use
/// [`build_composite_cover`] which auto-picks the layout.
pub fn build_daily_mix_cover(image_paths: &[PathBuf], metadata_dir: &Path) -> AppResult<String> {
    build_composite_cover(image_paths, metadata_dir)
}

/// Render a deterministic brand cover for a smart-playlist family that
/// has no per-track imagery (e.g. On Repeat — a fixed visual identity,
/// not a contact-sheet of the user's library). The output is a 640×640
/// JPEG with a diagonal gradient and a stylised infinity-loop motif —
/// no text, since the family label and playlist name render below the
/// tile on the Home view anyway.
///
/// Identical inputs always produce the same JPEG bytes (and therefore
/// the same blake3 hash), so a regen pass against an unchanged family
/// dedupes against the existing file in the shared cache instead of
/// piling up orphans.
pub fn build_on_repeat_cover(metadata_dir: &Path) -> AppResult<String> {
    let canvas = render_on_repeat_canvas();
    let bytes = encode_jpeg(&canvas)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let out = metadata_artwork::path_for_hash(metadata_dir, &hash);
    if !out.exists() {
        std::fs::write(&out, &bytes)
            .map_err(|e| AppError::Audio(format!("smart cover write: {e}")))?;
    }
    Ok(hash)
}

/// Paint the On Repeat brand canvas: indigo→magenta diagonal gradient
/// background overlaid with two intersecting magenta rings that form an
/// infinity loop. Kept separate from [`build_on_repeat_cover`] so the
/// pixel-level behaviour can be unit-tested without touching the disk.
fn render_on_repeat_canvas() -> RgbImage {
    // Anchor colours sampled to read well next to the emerald accent the
    // Home tile uses for On Repeat's ring + gradient fallback.
    const TOP_LEFT: [f32; 3] = [29.0, 16.0, 64.0]; // deep indigo
    const BOTTOM_RIGHT: [f32; 3] = [60.0, 8.0, 92.0]; // royal violet
    const RING: [u8; 3] = [236, 72, 153]; // tailwind pink-500
    const RING_HIGHLIGHT: [u8; 3] = [244, 114, 182]; // tailwind pink-400

    let mut canvas: RgbImage = ImageBuffer::from_pixel(
        CANVAS_PX,
        CANVAS_PX,
        Rgb([TOP_LEFT[0] as u8, TOP_LEFT[1] as u8, TOP_LEFT[2] as u8]),
    );

    // Diagonal background gradient — top-left → bottom-right.
    let max_d = ((CANVAS_PX - 1) as f32) * 2.0;
    for y in 0..CANVAS_PX {
        for x in 0..CANVAS_PX {
            let t = ((x + y) as f32 / max_d).clamp(0.0, 1.0);
            let r = TOP_LEFT[0] + (BOTTOM_RIGHT[0] - TOP_LEFT[0]) * t;
            let g = TOP_LEFT[1] + (BOTTOM_RIGHT[1] - TOP_LEFT[1]) * t;
            let b = TOP_LEFT[2] + (BOTTOM_RIGHT[2] - TOP_LEFT[2]) * t;
            canvas.put_pixel(x, y, Rgb([r as u8, g as u8, b as u8]));
        }
    }

    // Infinity loop: two overlapping ring strokes centred horizontally.
    // Radius + thickness are tuned so the two lobes touch at the centre
    // without overlapping into a single oval — the same proportions
    // Spotify uses on its own On Repeat cover.
    let cy = CANVAS_PX as f32 * 0.55;
    let radius = CANVAS_PX as f32 * 0.18;
    let cx_left = CANVAS_PX as f32 * 0.5 - radius * 0.95;
    let cx_right = CANVAS_PX as f32 * 0.5 + radius * 0.95;
    let stroke = CANVAS_PX as f32 * 0.05;
    let inner = radius - stroke * 0.5;
    let outer = radius + stroke * 0.5;

    for y in 0..CANVAS_PX {
        for x in 0..CANVAS_PX {
            let dx_l = x as f32 - cx_left;
            let dy = y as f32 - cy;
            let dx_r = x as f32 - cx_right;
            let dist_l = (dx_l * dx_l + dy * dy).sqrt();
            let dist_r = (dx_r * dx_r + dy * dy).sqrt();
            let in_left = dist_l >= inner && dist_l <= outer;
            let in_right = dist_r >= inner && dist_r <= outer;
            if in_left || in_right {
                // Highlight the upper arc to fake a soft top-light and
                // give the ring some depth instead of reading as a flat
                // sticker.
                let highlight = dy < 0.0;
                let color = if highlight { RING_HIGHLIGHT } else { RING };
                canvas.put_pixel(x, y, Rgb(color));
            }
        }
    }

    apply_bottom_gradient(&mut canvas);
    canvas
}

/// Slice the canvas into N equal vertical strips, centre-crop each source
/// image to fill its strip, and paint into the output buffer. Errors when
/// `strips` is empty so callers never silently render an all-black square
/// — the public entry point pre-checks too, but defending here keeps the
/// helper safe for future callers.
fn composite_strips(strips: &[RgbImage]) -> AppResult<RgbImage> {
    if strips.is_empty() {
        return Err(AppError::Audio(
            "smart cover: composite_strips requires at least one strip".into(),
        ));
    }
    let n = strips.len() as u32;
    let strip_w = CANVAS_PX / n;
    // Account for integer-division remainder by widening the last strip to
    // cover the full canvas — otherwise a 640/3 layout leaves a 1 px black
    // sliver on the right edge.
    let mut canvas = ImageBuffer::from_pixel(CANVAS_PX, CANVAS_PX, Rgb([18, 18, 18]));
    for (i, src) in strips.iter().enumerate() {
        let dst_x0 = (i as u32) * strip_w;
        let dst_w = if i + 1 == strips.len() {
            CANVAS_PX - dst_x0
        } else {
            strip_w
        };
        let resized = cover_fit(src, dst_w, CANVAS_PX)?;
        for y in 0..CANVAS_PX {
            for x in 0..dst_w {
                let p = *resized.get_pixel(x, y);
                canvas.put_pixel(dst_x0 + x, y, p);
            }
        }
    }
    apply_bottom_gradient(&mut canvas);
    Ok(canvas)
}

/// 2×2 grid composite — top-left, top-right, bottom-left, bottom-right —
/// at exactly 4 input tiles. Each cell is a centre-cropped square so album
/// covers (which are nearly always square anyway) drop in without
/// distortion. Used for user-playlist auto-covers à la Spotify; the smart
/// playlist family takes the strips path for 1-3 inputs.
fn composite_grid_2x2(tiles: &[RgbImage]) -> AppResult<RgbImage> {
    if tiles.len() < 4 {
        return Err(AppError::Audio(
            "smart cover: composite_grid_2x2 requires 4 tiles".into(),
        ));
    }
    let cell = CANVAS_PX / 2;
    let mut canvas = ImageBuffer::from_pixel(CANVAS_PX, CANVAS_PX, Rgb([18, 18, 18]));
    // Quadrant order matches reading order (TL, TR, BL, BR) so the strip
    // sequence reflects the playlist's first-4-tracks ordering.
    let positions = [(0, 0), (cell, 0), (0, cell), (cell, cell)];
    for (i, (dx, dy)) in positions.iter().enumerate() {
        let resized = cover_fit(&tiles[i], cell, cell)?;
        for y in 0..cell {
            for x in 0..cell {
                let p = *resized.get_pixel(x, y);
                canvas.put_pixel(dx + x, dy + y, p);
            }
        }
    }
    apply_bottom_gradient(&mut canvas);
    Ok(canvas)
}

/// Centre-crop `src` to the `dst_w × dst_h` aspect ratio, then SIMD-resize
/// to that exact size. Mirrors CSS `object-fit: cover`.
fn cover_fit(src: &RgbImage, dst_w: u32, dst_h: u32) -> AppResult<RgbImage> {
    let (sw, sh) = (src.width(), src.height());
    if sw == 0 || sh == 0 {
        return Err(AppError::Audio("smart cover: empty source image".into()));
    }
    let src_ratio = sw as f32 / sh as f32;
    let dst_ratio = dst_w as f32 / dst_h as f32;
    // Pick the largest centred sub-rect that matches dst's aspect ratio.
    let (crop_w, crop_h) = if src_ratio > dst_ratio {
        // Source is wider than target — crop the sides.
        ((sh as f32 * dst_ratio) as u32, sh)
    } else {
        // Source is taller (or equal) — crop the top/bottom.
        (sw, (sw as f32 / dst_ratio) as u32)
    };
    let crop_w = crop_w.max(1).min(sw);
    let crop_h = crop_h.max(1).min(sh);
    let crop_x = (sw - crop_w) / 2;
    let crop_y = (sh - crop_h) / 2;
    let cropped = image::imageops::crop_imm(src, crop_x, crop_y, crop_w, crop_h).to_image();

    // SIMD resize via fast_image_resize. The crate wants its own image type;
    // we hand it the raw RGB buffer and read the result back into an
    // `ImageBuffer` for the compositing step.
    let src_w_nz = NonZeroU32::new(crop_w).expect("crop_w > 0");
    let src_h_nz = NonZeroU32::new(crop_h).expect("crop_h > 0");
    let dst_w_nz = NonZeroU32::new(dst_w).expect("dst_w > 0");
    let dst_h_nz = NonZeroU32::new(dst_h).expect("dst_h > 0");
    let _ = (src_w_nz, src_h_nz, dst_w_nz, dst_h_nz); // crate API uses u32 directly in v6
    let src_fir = FirImage::from_vec_u8(crop_w, crop_h, cropped.into_raw(), PixelType::U8x3)
        .map_err(|e| AppError::Audio(format!("smart cover: fir from src: {e}")))?;
    let mut dst_fir = FirImage::new(dst_w, dst_h, PixelType::U8x3);
    let mut resizer = Resizer::new();
    resizer
        .resize(&src_fir, &mut dst_fir, &ResizeOptions::default())
        .map_err(|e| AppError::Audio(format!("smart cover: resize: {e}")))?;
    let resized = ImageBuffer::<Rgb<u8>, _>::from_raw(dst_w, dst_h, dst_fir.into_vec())
        .ok_or_else(|| AppError::Audio("smart cover: rebuild ImageBuffer".into()))?;
    Ok(resized)
}

/// Darken the bottom 40 % of the canvas with a smooth ease-out gradient so
/// the playlist title rendered on top by the frontend stays legible. The
/// curve is squared (`t²`) so the top of the gradient blends in instead of
/// showing a hard line.
fn apply_bottom_gradient(canvas: &mut RgbImage) {
    let h = canvas.height() as f32;
    let start = h * 0.6;
    for y in (start as u32)..canvas.height() {
        let t = ((y as f32 - start) / (h - start)).clamp(0.0, 1.0);
        let alpha = (t * t * 0.55).min(0.55);
        let one_minus = 1.0 - alpha;
        for x in 0..canvas.width() {
            let p = canvas.get_pixel_mut(x, y);
            p[0] = (p[0] as f32 * one_minus) as u8;
            p[1] = (p[1] as f32 * one_minus) as u8;
            p[2] = (p[2] as f32 * one_minus) as u8;
        }
    }
}

fn encode_jpeg(canvas: &RgbImage) -> AppResult<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::with_capacity(96 * 1024);
    let mut cursor = std::io::Cursor::new(&mut buf);
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, JPEG_QUALITY);
    canvas
        .write_with_encoder(encoder)
        .map_err(|e| AppError::Audio(format!("smart cover encode: {e}")))?;
    let _ = ImageFormat::Jpeg; // assert symbol use
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, color: [u8; 3]) -> RgbImage {
        ImageBuffer::from_pixel(w, h, Rgb(color))
    }

    #[test]
    fn composite_three_strips_fills_canvas() {
        let strips = vec![
            solid(800, 800, [200, 50, 50]),
            solid(800, 800, [50, 200, 50]),
            solid(800, 800, [50, 50, 200]),
        ];
        let canvas = composite_strips(&strips).expect("composite");
        assert_eq!(canvas.width(), CANVAS_PX);
        assert_eq!(canvas.height(), CANVAS_PX);
        // The top row should sample from each colour band exactly once,
        // proving every strip got painted (not just the last).
        let r = canvas.get_pixel(CANVAS_PX / 6, 10)[0];
        let g = canvas.get_pixel(CANVAS_PX / 2, 10)[1];
        let b = canvas.get_pixel(5 * CANVAS_PX / 6, 10)[2];
        assert!(r > 150 && g > 150 && b > 150);
    }

    #[test]
    fn cover_fit_handles_landscape_source() {
        // Wider than tall — the centre-crop should keep the middle band.
        let mut src = ImageBuffer::from_pixel(400, 100, Rgb([0, 0, 0]));
        for x in 150..250 {
            for y in 0..100 {
                src.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
        let out = cover_fit(&src, 100, 100).expect("fit");
        assert_eq!(out.width(), 100);
        assert_eq!(out.height(), 100);
        // The centre-crop preserved the white band.
        assert!(out.get_pixel(50, 50)[0] > 200);
    }

    #[test]
    fn empty_input_errors() {
        let strips: Vec<RgbImage> = vec![];
        assert!(composite_strips(&strips).is_err());
    }

    #[test]
    fn grid_2x2_paints_all_four_quadrants() {
        let tiles = vec![
            solid(800, 800, [200, 50, 50]),  // TL red
            solid(800, 800, [50, 200, 50]),  // TR green
            solid(800, 800, [50, 50, 200]),  // BL blue
            solid(800, 800, [200, 200, 50]), // BR yellow
        ];
        let canvas = composite_grid_2x2(&tiles).expect("grid");
        assert_eq!(canvas.width(), CANVAS_PX);
        let q = CANVAS_PX / 4;
        // Sample the centre of each quadrant — colours should match.
        assert!(canvas.get_pixel(q, q)[0] > 150, "TL red");
        assert!(canvas.get_pixel(3 * q, q)[1] > 150, "TR green");
        assert!(canvas.get_pixel(q, 3 * q)[2] > 150, "BL blue");
        let br = canvas.get_pixel(3 * q, 3 * q);
        assert!(br[0] > 100 && br[1] > 100, "BR yellow");
    }

    #[test]
    fn grid_2x2_rejects_under_four_tiles() {
        let tiles = vec![solid(100, 100, [255, 255, 255]); 3];
        assert!(composite_grid_2x2(&tiles).is_err());
    }

    #[test]
    fn on_repeat_canvas_paints_background_and_ring() {
        let canvas = render_on_repeat_canvas();
        assert_eq!(canvas.width(), CANVAS_PX);
        assert_eq!(canvas.height(), CANVAS_PX);
        // Top-left corner should have at least some blue from the
        // indigo background — i.e. not all-black, not all-magenta.
        let tl = canvas.get_pixel(8, 8);
        assert!(tl[2] > 40, "expected indigo blue in top-left, got {tl:?}");
        // A pixel on the ring (top arc, left lobe) should be pink-ish:
        // strong red, low green relative to red.
        let r_x = (CANVAS_PX as f32 * 0.31) as u32;
        let r_y = (CANVAS_PX as f32 * 0.39) as u32;
        let ring = canvas.get_pixel(r_x, r_y);
        assert!(
            ring[0] > 200 && ring[1] < 200,
            "expected pink ring pixel at ({r_x},{r_y}), got {ring:?}"
        );
    }

    #[test]
    fn gradient_darkens_bottom_not_top() {
        let mut canvas = solid(CANVAS_PX, CANVAS_PX, [200, 200, 200]);
        apply_bottom_gradient(&mut canvas);
        // Top untouched.
        assert_eq!(canvas.get_pixel(0, 0)[0], 200);
        // Bottom row darker than start of gradient.
        let bottom = canvas.get_pixel(0, CANVAS_PX - 1)[0];
        assert!(bottom < 200, "expected darkening, got {bottom}");
    }

    #[test]
    fn composite_collapses_identical_inputs_to_single_tile() {
        // Three identical paths must produce the same cover as a single
        // path — anything else means the Daily Mix carousel would still
        // show a 3-strip contact sheet of the same picture.
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("artist.jpg");
        let img = solid(640, 640, [180, 120, 60]);
        img.save_with_format(&src, ImageFormat::Jpeg)
            .expect("write source jpg");

        let hash_dup = build_composite_cover(&[src.clone(), src.clone(), src.clone()], dir.path())
            .expect("dup composite");
        let hash_single = build_composite_cover(&[src.clone()], dir.path()).expect("single composite");
        assert_eq!(
            hash_dup, hash_single,
            "duplicate inputs should collapse to the single-tile composite"
        );
    }
}
