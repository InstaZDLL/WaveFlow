-- Covering / partial indexes for the library browse queries.
--
-- list_albums, list_artists, list_genres all share the same shape:
--   JOIN track t ON t.<fk> = parent.id
--   WHERE t.is_available = 1
--   GROUP BY parent.id
-- The pre-existing single-column indexes (idx_track_album,
-- idx_track_primary_artist, idx_track_artist_artist, etc.) seek to the
-- right rows but force a table fetch per row to test is_available.
-- These partial indexes pre-filter on is_available = 1 (the >99% case
-- for healthy libraries) so the GROUP BY can walk a tight index range.
--
-- Replay-safe via IF NOT EXISTS.

-- list_albums: scans tracks per album, summing duration_ms and taking
-- MAX(bit_depth)/MAX(sample_rate). The partial index narrows to
-- available tracks; the included columns let SQLite resolve duration
-- + quality without touching the row.
CREATE INDEX IF NOT EXISTS idx_track_album_available
    ON track(album_id, duration_ms, bit_depth, sample_rate)
    WHERE is_available = 1;

-- list_artists: walks track_artist→track per artist, counting distinct
-- track + album. Partial filter on availability keeps the index slim.
CREATE INDEX IF NOT EXISTS idx_track_primary_artist_available
    ON track(primary_artist)
    WHERE is_available = 1;

-- list_genres: same shape as list_artists for genre.
CREATE INDEX IF NOT EXISTS idx_track_genre_track
    ON track_genre(track_id, genre_id);
