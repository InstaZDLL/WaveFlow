-- The track and disc totals a file carries (`TRCK` "4/12", `TRACKTOTAL`,
-- `trkn`; `TPOS` "1/2", `DISCTOTAL`, `disk`), for the album completeness
-- check: numbering alone cannot tell a nine-track album from a ten-track
-- album missing its last track, and the total can.
--
-- On the track, not the album. `album.total_tracks` exists since the
-- initial schema but was never written, and it cannot hold the answer:
-- a track total is the size of *its disc*, so a double album carries two,
-- and one album-level column would keep the larger and report the
-- shorter disc as incomplete.
--
-- Additive, NULL by default: a library scanned before this fills in on
-- the next scan of each folder, through the scanner's one-off totals pass.
ALTER TABLE track ADD COLUMN track_total INTEGER;
ALTER TABLE track ADD COLUMN disc_total INTEGER;
