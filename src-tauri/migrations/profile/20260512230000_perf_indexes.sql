-- Performance indexes for large libraries (50k+ tracks).
--
-- These are append-only "preventive" indexes: each helper query
-- below ran as a table scan before — fine on small libraries
-- (sub-ms) but linear with track count once analysis covers the
-- bulk of a 50k+ collection.
--
-- `IF NOT EXISTS` keeps the migration replay-safe.

-- mood_radio + radio (start_radio) filter tracks on tempo windows
-- (e.g. WHERE bpm BETWEEN 60 AND 110). Without an index this is a
-- full scan of `track_analysis`; with one it becomes a range probe.
CREATE INDEX IF NOT EXISTS idx_track_analysis_bpm
    ON track_analysis(bpm)
    WHERE bpm IS NOT NULL;

-- Focus / Sleep mood radios add a loudness ceiling filter
-- (WHERE loudness_lufs <= ?). Partial index keeps the index small
-- — most rows have a LUFS value once analysis ran, but rows
-- without it would otherwise carry a NULL key entry for no gain.
CREATE INDEX IF NOT EXISTS idx_track_analysis_lufs
    ON track_analysis(loudness_lufs)
    WHERE loudness_lufs IS NOT NULL;

-- Smart playlists with a `rating_min` predicate compare against
-- track.rating. Partial index — unrated tracks are the majority
-- of a fresh import and don't need to be in the key set.
CREATE INDEX IF NOT EXISTS idx_track_rating
    ON track(rating)
    WHERE rating IS NOT NULL;
