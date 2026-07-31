-- Per-track looping Canvas (issue #442) — a short muted mp4 the user sets
-- on a single track, played behind the now-playing view Spotify-Canvas
-- style. Keyed per TRACK (not album), matching Spotify: the clip belongs to
-- the current song. The row's existence is itself the "has a Canvas" signal;
-- nothing else writes this table.
--
-- A fresh CREATE TABLE (not an ALTER on `track`), so it carries none of the
-- DROP TABLE / foreign-key cascade risk documented in CLAUDE.md for parent
-- tables. `ON DELETE CASCADE` drops the row when the track is removed; the
-- on-disk mp4 in the never-evicted `canvas/` dir is GC'd by a future pass,
-- the same tradeoff `album_motion_artwork` (issue #408) takes.
CREATE TABLE track_canvas (
    track_id   INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    hash       TEXT NOT NULL,
    format     TEXT NOT NULL DEFAULT 'mp4',
    created_at INTEGER NOT NULL
);
