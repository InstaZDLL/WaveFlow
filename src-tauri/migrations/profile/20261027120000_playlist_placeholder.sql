-- Entries of an imported playlist that the library does not hold (yet).
--
-- A table of its own rather than a nullable `track_id` in `playlist_track`:
-- that table's primary key is `(playlist_id, track_id)`, and every reader of
-- it — smart playlists, the M3U export, the conversion to a server playlist,
-- the auto-cover, the counts — would have to learn to skip a row that names
-- no track. Here they see nothing, which is what a placeholder is to them.
--
-- `position` lives in `playlist_track.position`'s numbering: a placeholder
-- sits just before the real track holding the same position (or at the tail
-- when no track does), and `ordinal` orders the placeholders of one gap.
-- Every write that shifts `playlist_track.position` shifts these the same
-- way, in the same transaction. When a track the scan brings in matches one,
-- the track takes its place in `playlist_track` and the row goes.
CREATE TABLE playlist_placeholder (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_id INTEGER NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    ordinal     INTEGER NOT NULL DEFAULT 0,
    title       TEXT NOT NULL,
    artist      TEXT,
    album       TEXT,
    duration_ms INTEGER,
    -- Upper-case, no separators: the form the matcher compares.
    isrc        TEXT,
    -- The path an M3U entry named, kept so the file turning up there (or
    -- under the same name elsewhere) is recognised like at import time.
    locator     TEXT,
    source      TEXT NOT NULL CHECK (source IN ('m3u', 'csv', 'json', 'deezer')),
    added_at    INTEGER NOT NULL
);

CREATE INDEX idx_playlist_placeholder_order
    ON playlist_placeholder(playlist_id, position, ordinal);
