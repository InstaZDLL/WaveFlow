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
fn on_repeat_canvas_renders_with_opaque_background() {
    let canvas = render_on_repeat_canvas().expect("render");
    assert_eq!(canvas.width(), CANVAS_PX);
    assert_eq!(canvas.height(), CANVAS_PX);

    // Top-left should be the indigo gradient start (#240c47 → R≈36, B≈71)
    // — significantly more blue than red, never near-black.
    let tl = canvas.get_pixel(8, 8);
    assert!(
        tl[2] as i32 > tl[0] as i32,
        "expected indigo (B > R) in top-left, got {tl:?}"
    );
    assert!(tl[2] > 40, "top-left looks too dark: {tl:?}");

    // Bottom-right should be the gradient end (#0d041a) — basically
    // black-ish, well below the top-left brightness.
    let br = canvas.get_pixel(CANVAS_PX - 8, CANVAS_PX - 8);
    let br_sum = br[0] as u32 + br[1] as u32 + br[2] as u32;
    assert!(
        br_sum < 100,
        "expected near-black bottom-right gradient, got {br:?}"
    );

    // The horizontal slice through the ring centre must contain at
    // least one strongly pink pixel from the bezier infinity loop
    // (gradient start is #ff3377). Anti-aliasing + glow means we
    // can't pin a specific x, so we scan the row.
    let ring_y = CANVAS_PX / 2; // loop is centred vertically (viewBox y=250)
    let mut max_red = 0_u8;
    for x in 0..CANVAS_PX {
        let p = canvas.get_pixel(x, ring_y);
        max_red = max_red.max(p[0]);
    }
    assert!(
        max_red > 200,
        "expected at least one pink ring pixel on row {ring_y}, max red was {max_red}"
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
    let hash_single =
        build_composite_cover(std::slice::from_ref(&src), dir.path()).expect("single composite");
    assert_eq!(
        hash_dup, hash_single,
        "duplicate inputs should collapse to the single-tile composite"
    );
}
