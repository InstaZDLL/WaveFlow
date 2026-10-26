//! The spectrum over time, for Track Properties: what the fake-lossless
//! verdict ([`super::bandwidth`]) averages away, so the user can see the
//! cut for themselves.
//!
//! Recorded by the same [`super::bandwidth::BandwidthMeter`] pass — the
//! same 4096-point Hann frames at the native rate, the channels' powers
//! added rather than a mono sum — so a wall in the picture is the wall
//! the verdict measured, not one a different analysis drew. Silent frames,
//! which the verdict leaves out of its average, are kept here: a gap in
//! time should show as a gap.
//!
//! Each column averages the power of a run of frames, each row a band of
//! bins on a linear scale from 0 Hz to Nyquist (linear, because the cut
//! being looked for sits in the top octave, which a log scale squeezes
//! into a sliver). Cells are stored as one byte over a fixed range under
//! the loudest cell.

/// Rows in the picture: 8 bins of a 4096-point frame each, 0 Hz at row 0.
pub const ROWS: usize = 256;

/// Range below the loudest cell that the byte scale covers, in dB. A
/// lossy encoder's cut falls 20 dB and more (`bandwidth::WALL_DB`), and
/// the music it cuts sits 40-80 dB under the peak; 120 keeps the noise
/// floor of a 16-bit file in view without crushing the rest.
pub const RANGE_DB: f64 = 120.0;

/// Lowest level the scale is ever measured from, in dB, under the noise
/// floor of a 24-bit file. Without it a silent file, every cell at the
/// -300 dB clamp, is measured against itself and drawn at full level.
const FLOOR_DB: f64 = -130.0;

/// What [`SpectrogramRecorder::finish`] produces.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Spectrogram {
    pub sample_rate: u32,
    pub columns: usize,
    pub rows: usize,
    /// Audio covered by one column, in seconds.
    pub seconds_per_column: f64,
    /// Audio covered by the whole picture, in seconds: the last column
    /// is usually only partly filled, so this is not `columns ×
    /// seconds_per_column`.
    pub duration_seconds: f64,
    /// Column-major, `rows` bytes per column, row 0 = 0 Hz. `0` is
    /// [`RANGE_DB`] or more under the loudest cell, `255` the loudest.
    pub cells: Vec<u8>,
}

/// Folds frame spectra into columns. Fed by `BandwidthMeter` with the
/// power of each frame, one value per FFT bin.
pub(crate) struct SpectrogramRecorder {
    rate: f64,
    frame_len: usize,
    frames_per_column: usize,
    /// The most columns kept. The run of frames per column is planned
    /// from the length the container declares, which can be missing or
    /// wrong; past this many, neighbouring columns are merged and the run
    /// doubles, so a file that lies about its length cannot grow the
    /// picture (or the memory behind it) without bound.
    max_columns: usize,
    acc: Vec<f64>,
    in_column: usize,
    /// Frames pushed in all.
    frames: u64,
    /// Mean power per row, column after column.
    power: Vec<f64>,
}

impl SpectrogramRecorder {
    pub(crate) fn new(
        rate: f64,
        frame_len: usize,
        frames_per_column: usize,
        max_columns: usize,
    ) -> Self {
        Self {
            rate,
            frame_len,
            frames_per_column: frames_per_column.max(1),
            max_columns: max_columns.max(2),
            acc: vec![0.0; ROWS],
            in_column: 0,
            frames: 0,
            power: Vec::new(),
        }
    }

    /// One frame's power per bin (`frame_len / 2 + 1` values; the
    /// Nyquist bin is left out so the rows split the rest evenly).
    pub(crate) fn push(&mut self, bins: &[f64]) {
        let per_row = (bins.len().saturating_sub(1) / ROWS).max(1);
        for (row, acc) in self.acc.iter_mut().enumerate() {
            let lo = row * per_row;
            let hi = (lo + per_row).min(bins.len());
            *acc += bins[lo.min(hi)..hi].iter().sum::<f64>() / per_row as f64;
        }
        self.in_column += 1;
        self.frames += 1;
        if self.in_column == self.frames_per_column {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.in_column == 0 {
            return;
        }
        let n = self.in_column as f64;
        self.power.extend(self.acc.iter().map(|p| p / n));
        self.acc.iter_mut().for_each(|p| *p = 0.0);
        self.in_column = 0;
        if self.power.len() / ROWS > self.max_columns {
            self.halve();
        }
    }

    /// Merge neighbouring columns and double the run per column. An odd
    /// column out goes back into the accumulator as a half-filled column
    /// of the new run, so every finished column covers the same time.
    fn halve(&mut self) {
        let columns = self.power.len() / ROWS;
        if columns % 2 == 1 {
            let last = self.power.split_off((columns - 1) * ROWS);
            for (acc, p) in self.acc.iter_mut().zip(&last) {
                *acc = p * self.frames_per_column as f64;
            }
            self.in_column = self.frames_per_column;
        }
        let merged: Vec<f64> = self
            .power
            .chunks_exact(2 * ROWS)
            .flat_map(|pair| {
                let (a, b) = pair.split_at(ROWS);
                a.iter()
                    .zip(b)
                    .map(|(x, y)| (x + y) / 2.0)
                    .collect::<Vec<_>>()
            })
            .collect();
        self.power = merged;
        self.frames_per_column *= 2;
    }

    /// `None` when not one frame was recorded.
    pub(crate) fn finish(mut self) -> Option<Spectrogram> {
        self.flush();
        // The last flush can tip the picture over the cap, and halving
        // then hands an odd column back to the accumulator: flush it
        // again, or the end of the track would be missing.
        if self.in_column > 0 {
            self.flush();
        }
        if self.power.is_empty() {
            return None;
        }
        let db: Vec<f64> = self
            .power
            .iter()
            .map(|&p| 10.0 * p.max(1e-30).log10())
            .collect();
        let top = db.iter().copied().fold(FLOOR_DB, f64::max);
        let cells = db
            .iter()
            .map(|&d| (((d - (top - RANGE_DB)) / RANGE_DB).clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect();
        Some(Spectrogram {
            sample_rate: self.rate.round() as u32,
            columns: self.power.len() / ROWS,
            rows: ROWS,
            seconds_per_column: (self.frames_per_column * self.frame_len) as f64 / self.rate,
            duration_seconds: (self.frames * self.frame_len as u64) as f64 / self.rate,
            cells,
        })
    }
}

/// Frames per column for about `width` columns over `total_frames`
/// samples, cut into `frame_len`-sample FFT frames.
pub fn frames_per_column(total_frames: u64, frame_len: usize, width: usize) -> usize {
    let ffts = total_frames.div_ceil(frame_len as u64).max(1);
    (ffts.div_ceil(width.max(1) as u64) as usize).max(1)
}

#[cfg(test)]
mod tests {
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
}
