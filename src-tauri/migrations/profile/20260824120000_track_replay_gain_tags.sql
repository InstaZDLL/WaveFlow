-- ReplayGain values carried by the file's own tags.
--
-- These belong on `track` rather than on `track_analysis` because
-- they are a property of the file, refreshed by the scanner on every
-- (re)scan, whereas `track_analysis` holds what *we* measured and is
-- only written by an explicit analysis pass. Keeping them apart is
-- what lets playback prefer the tag and fall back to our measurement
-- without either one overwriting the other.
--
-- Gains are in dB on the ReplayGain 2.0 scale (-18 LUFS reference);
-- an R128 tag is converted to that scale at scan time. Peaks are
-- linear, normally 0..1 but legitimately above 1.0 on clipped
-- masters with intersample peaks.
ALTER TABLE track ADD COLUMN rg_track_gain_db REAL;
ALTER TABLE track ADD COLUMN rg_track_peak REAL;
ALTER TABLE track ADD COLUMN rg_album_gain_db REAL;
ALTER TABLE track ADD COLUMN rg_album_peak REAL;
