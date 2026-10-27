use super::*;

#[test]
fn columns_average_their_frames_and_rows_cover_the_bins() {
    // 4096-point frames: 2049 bins, 8 per row.
    let mut rec = SpectrogramRecorder::new(44_100.0, 4096, 2, 900);
    let mut loud_low = vec![0.0; 2049];
    loud_low[..8].iter_mut().for_each(|b| *b = 1.0);
    rec.push(&loud_low);
    rec.push(&vec![0.0; 2049]);
    rec.push(&loud_low);
    let out = rec.finish().unwrap();
    // Two full frames, then a column of one: three frames, two columns.
    assert_eq!(out.columns, 2);
    assert_eq!(out.rows, ROWS);
    assert_eq!(out.cells.len(), 2 * ROWS);
    // The last column holds only loud frames: the loudest cell.
    assert_eq!(out.cells[ROWS], 255);
    // The first averaged a loud frame with a silent one: 3 dB lower.
    let expected = ((RANGE_DB - 10.0 * 2f64.log10()) / RANGE_DB * 255.0).round() as u8;
    assert_eq!(out.cells[0], expected);
    // Nothing above the first row.
    assert_eq!(out.cells[1], 0);
    assert!((out.seconds_per_column - 2.0 * 4096.0 / 44_100.0).abs() < 1e-9);
    // Three frames, not two full columns' worth.
    assert!((out.duration_seconds - 3.0 * 4096.0 / 44_100.0).abs() < 1e-9);
}

#[test]
fn frames_per_column_spreads_a_track_over_the_width() {
    // Five minutes at 44.1 kHz: 3230 frames of 4096 over 900 columns.
    assert_eq!(frames_per_column(44_100 * 300, 4096, 900), 4);
    // A track shorter than the width: one frame per column.
    assert_eq!(frames_per_column(44_100 * 10, 4096, 900), 1);
    assert_eq!(frames_per_column(0, 4096, 900), 1);
}

#[test]
fn a_length_that_lies_cannot_grow_the_picture() {
    // Planned for one frame per column, capped at 8 columns, fed 100
    // frames: 100 columns would be 25 600 cells.
    let mut rec = SpectrogramRecorder::new(44_100.0, 4096, 1, 8);
    for i in 0..100 {
        let level = if i < 50 { 1.0 } else { 0.0 };
        rec.push(&vec![level; 2049]);
    }
    let out = rec.finish().unwrap();
    assert!(out.columns <= 9, "{} columns", out.columns);
    assert_eq!(out.cells.len(), out.columns * ROWS);
    // Each column still covers the same time, and time keeps its
    // order: loud first, silent last.
    let per_column = out.seconds_per_column * 44_100.0 / 4096.0;
    assert_eq!(per_column.round() as usize, 16);
    assert_eq!(out.cells[0], 255);
    assert_eq!(out.cells[(out.columns - 1) * ROWS], 0);
}

#[test]
fn the_last_frames_survive_a_final_halving() {
    // 17 frames, two per column, capped at 8: the closing flush makes
    // a ninth column and halves.
    let mut rec = SpectrogramRecorder::new(44_100.0, 4096, 2, 8);
    for i in 0..17 {
        rec.push(&vec![if i == 16 { 1.0 } else { 0.0 }; 2049]);
    }
    let out = rec.finish().unwrap();
    // The loud last frame is still in the picture.
    assert_eq!(out.cells[(out.columns - 1) * ROWS], 255);
    assert!((out.duration_seconds - 17.0 * 4096.0 / 44_100.0).abs() < 1e-9);
}

#[test]
fn nothing_recorded_is_nothing_to_show() {
    assert!(SpectrogramRecorder::new(48_000.0, 4096, 4, 900)
        .finish()
        .is_none());
}
