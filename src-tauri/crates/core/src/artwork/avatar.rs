//! A profile's picture: an image the user picked on their disk, made
//! into the one shape every surface draws.
//!
//! The file is hostile input. It comes from anywhere on the disk, and
//! from a `.waveflow` archive someone else may have written, so before a
//! decoder sees it:
//!
//! - its size is capped ([`MAX_AVATAR_SOURCE_BYTES`]);
//! - its format is read from its bytes, never from its name, and only
//!   PNG, JPEG, WebP and GIF are accepted;
//! - a canvas over [`MAX_AVATAR_PIXELS`] is refused from its header,
//!   before a pixel is read, and the decoder runs under [`image::Limits`]
//!   (at most [`MAX_AVATAR_DIMENSION`] pixels a side and a bounded
//!   allocation), so a small file that announces a huge canvas is never
//!   allocated;
//! - only the centre square is kept past the decode, and it is turned
//!   upright after it is cut out, so what is held at once stays within
//!   about twice the decoded picture.
//!
//! What comes out is a [`AVATAR_SIZE`]-pixel square PNG, the centre of
//! the picture, turned upright when a JPEG or WebP says it was taken on
//! its side. An animated GIF or WebP keeps its first frame. The original
//! is never kept, and neither is its path.

use std::io::Cursor;

use fast_image_resize as fr;
use fr::{ResizeOptions, Resizer};
use image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};

/// Side of the stored square, in pixels. Twice the largest place it is
/// drawn (the 96 px tiles of the profile selector), so a HiDPI screen
/// still gets a sharp picture.
pub const AVATAR_SIZE: u32 = 256;

/// The largest file read as a picture. A phone photo is a few
/// megabytes; anything past this is not a profile picture.
pub const MAX_AVATAR_SOURCE_BYTES: u64 = 20 * 1024 * 1024;

/// The widest and tallest picture decoded; a header past this is refused
/// before any pixel is read.
pub const MAX_AVATAR_DIMENSION: u32 = 10_000;

/// The most pixels decoded. A 48-megapixel photo (8 000 × 6 000) fits;
/// its decode is 144 MB in RGB, which bounds everything after it.
pub const MAX_AVATAR_PIXELS: u64 = 50_000_000;

/// The most the decoder may allocate at once: the pixel budget in RGBA,
/// so a 16-bit picture of that size is refused rather than doubled.
const MAX_AVATAR_DECODE_ALLOC: u64 = MAX_AVATAR_PIXELS * 4 + 16 * 1024 * 1024;

/// Why a file could not become a picture. Each one is a sentence the
/// interface shows, so they stay few and say what the user can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarRefusal {
    /// Over [`MAX_AVATAR_SOURCE_BYTES`], or a canvas over the limits.
    TooLarge,
    /// Not a PNG, JPEG, WebP or GIF.
    Unsupported,
    /// One of those, but damaged or cut short.
    Unreadable,
}

impl AvatarRefusal {
    /// The stable word the backend's error carries, which the frontend
    /// matches to a translated sentence.
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "too-large",
            Self::Unsupported => "unsupported",
            Self::Unreadable => "unreadable",
        }
    }
}

impl std::fmt::Display for AvatarRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "avatar: {}", self.code())
    }
}

impl std::error::Error for AvatarRefusal {}

/// Make the picture a profile keeps out of the bytes of a file.
///
/// Returns the PNG to store. Every failure is one of the three
/// [`AvatarRefusal`]s; nothing here panics on a crafted file.
pub fn normalize_avatar(bytes: &[u8]) -> Result<Vec<u8>, AvatarRefusal> {
    if bytes.len() as u64 > MAX_AVATAR_SOURCE_BYTES {
        return Err(AvatarRefusal::TooLarge);
    }
    let format = image::guess_format(bytes).map_err(|_| AvatarRefusal::Unsupported)?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        return Err(AvatarRefusal::Unsupported);
    }

    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_AVATAR_DIMENSION);
    limits.max_image_height = Some(MAX_AVATAR_DIMENSION);
    limits.max_alloc = Some(MAX_AVATAR_DECODE_ALLOC);
    reader.limits(limits);

    let mut decoder = reader.into_decoder().map_err(refusal_of)?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_AVATAR_PIXELS {
        return Err(AvatarRefusal::TooLarge);
    }
    if width == 0 || height == 0 {
        return Err(AvatarRefusal::Unreadable);
    }
    // A photo taken on its side says so in its EXIF; drawn as stored, the
    // face would lie on its ear. A file whose orientation cannot be read
    // is drawn as stored rather than refused.
    let orientation = decoder.orientation().ok();
    let picture = DynamicImage::from_decoder(decoder).map_err(refusal_of)?;

    // The centre square, cut before turning: the centre square of a
    // picture is the centre square of any of its eight orientations, and
    // turning the square alone never copies the whole picture again.
    let side = picture.width().min(picture.height());
    let mut square = picture.crop_imm(
        (picture.width() - side) / 2,
        (picture.height() - side) / 2,
        side,
        side,
    );
    drop(picture);
    if let Some(orientation) = orientation {
        square.apply_orientation(orientation);
    }
    square_png(square)
}

fn refusal_of(err: ImageError) -> AvatarRefusal {
    match err {
        ImageError::Limits(_) => AvatarRefusal::TooLarge,
        ImageError::Unsupported(_) => AvatarRefusal::Unsupported,
        _ => AvatarRefusal::Unreadable,
    }
}

/// A square picture resized to [`AVATAR_SIZE`] and encoded as PNG (the
/// alpha of a transparent picture is kept).
fn square_png(square: DynamicImage) -> Result<Vec<u8>, AvatarRefusal> {
    let rgba = square.into_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(AvatarRefusal::Unreadable);
    }

    let source =
        fr::images::Image::from_vec_u8(width, height, rgba.into_raw(), fr::PixelType::U8x4)
            .map_err(|_| AvatarRefusal::Unreadable)?;
    let mut target = fr::images::Image::new(AVATAR_SIZE, AVATAR_SIZE, fr::PixelType::U8x4);
    Resizer::new()
        .resize(&source, &mut target, &ResizeOptions::new())
        .map_err(|_| AvatarRefusal::Unreadable)?;

    let square = image::RgbaImage::from_raw(AVATAR_SIZE, AVATAR_SIZE, target.into_vec())
        .ok_or(AvatarRefusal::Unreadable)?;
    let mut png = Cursor::new(Vec::new());
    square
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| AvatarRefusal::Unreadable)?;
    Ok(png.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn encode(img: &RgbaImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        match format {
            // JPEG has no alpha channel.
            ImageFormat::Jpeg => DynamicImage::ImageRgba8(img.clone())
                .to_rgb8()
                .write_to(&mut out, format)
                .unwrap(),
            _ => img.write_to(&mut out, format).unwrap(),
        }
        out.into_inner()
    }

    /// A wide picture: red on the outer quarters, blue in the middle half.
    fn banner(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, _| {
            if x < width / 4 || x >= width - width / 4 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        })
    }

    fn decoded(png: &[u8]) -> RgbaImage {
        assert_eq!(image::guess_format(png).unwrap(), ImageFormat::Png);
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    #[test]
    fn every_accepted_format_becomes_a_square_png() {
        let source = banner(64, 32);
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::WebP,
            ImageFormat::Gif,
        ] {
            let out = normalize_avatar(&encode(&source, format))
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            let img = decoded(&out);
            assert_eq!(img.dimensions(), (AVATAR_SIZE, AVATAR_SIZE), "{format:?}");
        }
    }

    #[test]
    fn a_wide_picture_keeps_its_centre() {
        // 800 × 400: the centre square is x 200..600, which is all blue.
        // The very edge is sampled with its neighbours outside the square,
        // so the check stays a few pixels in.
        let out = normalize_avatar(&encode(&banner(800, 400), ImageFormat::Png)).unwrap();
        let img = decoded(&out);
        for x in [4, AVATAR_SIZE / 2, AVATAR_SIZE - 5] {
            let px = img.get_pixel(x, AVATAR_SIZE / 2);
            assert!(px[2] > 200 && px[0] < 40, "x={x}: {px:?}");
        }
    }

    #[test]
    fn a_tall_picture_keeps_its_centre() {
        // 400 × 800, red on the top and bottom quarters.
        let tall = RgbaImage::from_fn(400, 800, |_, y| {
            if !(200..600).contains(&y) {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 255, 0, 255])
            }
        });
        let img = decoded(&normalize_avatar(&encode(&tall, ImageFormat::Png)).unwrap());
        for y in [4, AVATAR_SIZE - 5] {
            let px = img.get_pixel(AVATAR_SIZE / 2, y);
            assert!(px[1] > 200 && px[0] < 40, "y={y}: {px:?}");
        }
    }

    #[test]
    fn transparency_is_kept() {
        let clear = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        let img = decoded(&normalize_avatar(&encode(&clear, ImageFormat::Png)).unwrap());
        assert_eq!(img.get_pixel(10, 10)[3], 0);
    }

    #[test]
    fn other_formats_are_refused_from_their_bytes() {
        let bmp = encode(&banner(8, 8), ImageFormat::Bmp);
        assert_eq!(normalize_avatar(&bmp), Err(AvatarRefusal::Unsupported));
        let tiff = encode(&banner(8, 8), ImageFormat::Tiff);
        assert_eq!(normalize_avatar(&tiff), Err(AvatarRefusal::Unsupported));
        assert_eq!(
            normalize_avatar(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            Err(AvatarRefusal::Unsupported)
        );
        assert_eq!(normalize_avatar(&[]), Err(AvatarRefusal::Unsupported));
    }

    #[test]
    fn a_damaged_file_is_refused() {
        let png = encode(&banner(64, 64), ImageFormat::Png);
        // The signature and the header survive, the pixels do not.
        assert_eq!(
            normalize_avatar(&png[..png.len() / 2]),
            Err(AvatarRefusal::Unreadable)
        );
        let jpeg = encode(&banner(64, 64), ImageFormat::Jpeg);
        let mut garbled = jpeg[..40].to_vec();
        garbled.extend(std::iter::repeat_n(0xA5, 64));
        assert!(normalize_avatar(&garbled).is_err());
    }

    #[test]
    fn a_canvas_past_the_limit_is_refused_before_it_is_allocated() {
        // One pixel high and one past the limit wide: a few hundred bytes
        // on disk, refused from its header.
        let strip = RgbaImage::from_pixel(MAX_AVATAR_DIMENSION + 1, 1, Rgba([1, 2, 3, 255]));
        let png = encode(&strip, ImageFormat::Png);
        assert!((png.len() as u64) < MAX_AVATAR_SOURCE_BYTES);
        assert_eq!(normalize_avatar(&png), Err(AvatarRefusal::TooLarge));
    }

    #[test]
    fn a_canvas_past_the_pixel_budget_is_refused_from_its_header() {
        // 8 000 × 7 000 = 56 megapixels, each side under the limit. Only
        // the header is there: a decode would fail as unreadable, so the
        // refusal can only come from the header's size.
        let png = png_header_only(8_000, 7_000);
        assert_eq!(normalize_avatar(&png), Err(AvatarRefusal::TooLarge));
        // The same header within the budget is read past, and fails on
        // the missing pixels instead.
        let within = png_header_only(7_000, 7_000);
        assert_eq!(normalize_avatar(&within), Err(AvatarRefusal::Unreadable));
    }

    /// A PNG whose `IHDR` announces `width` × `height` and whose `IDAT`
    /// holds no pixel at all: a few dozen bytes.
    fn png_header_only(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        for (kind, data) in [
            (*b"IHDR", ihdr),
            // An empty zlib stream.
            (
                *b"IDAT",
                vec![0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
            ),
            (*b"IEND", Vec::new()),
        ] {
            let mut body = kind.to_vec();
            body.extend_from_slice(&data);
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(&body);
            png.extend_from_slice(&crc32(&body).to_be_bytes());
        }
        png
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn a_turned_square_is_the_centre_of_the_turned_picture() {
        // Cutting the centre square then turning it gives what turning
        // the whole picture then cutting would, for every orientation.
        use image::metadata::Orientation;
        let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(6, 4, |x, y| {
            Rgba([x as u8 * 40, y as u8 * 60, 7, 255])
        }));
        for orientation in [
            Orientation::NoTransforms,
            Orientation::Rotate90,
            Orientation::Rotate180,
            Orientation::Rotate270,
            Orientation::FlipHorizontal,
            Orientation::FlipVertical,
            Orientation::Rotate90FlipH,
            Orientation::Rotate270FlipH,
        ] {
            let mut cut_first = img.crop_imm(1, 0, 4, 4);
            cut_first.apply_orientation(orientation);
            let mut turned = img.clone();
            turned.apply_orientation(orientation);
            let (w, h) = (turned.width(), turned.height());
            let turned_first = turned.crop_imm((w - 4) / 2, (h - 4) / 2, 4, 4);
            assert_eq!(
                cut_first.to_rgba8(),
                turned_first.to_rgba8(),
                "{orientation:?}"
            );
        }
    }

    #[test]
    fn a_file_past_the_size_cap_is_refused_unread() {
        let mut big = encode(&banner(8, 8), ImageFormat::Png);
        big.resize(MAX_AVATAR_SOURCE_BYTES as usize + 1, 0);
        assert_eq!(normalize_avatar(&big), Err(AvatarRefusal::TooLarge));
    }

    #[test]
    fn a_tiny_picture_is_scaled_up_to_the_square() {
        let dot = RgbaImage::from_pixel(1, 1, Rgba([10, 200, 30, 255]));
        let img = decoded(&normalize_avatar(&encode(&dot, ImageFormat::Png)).unwrap());
        assert_eq!(img.dimensions(), (AVATAR_SIZE, AVATAR_SIZE));
    }
}
