-- Albums, artists and playlists pinned to the sidebar, one row per pin.
--
-- A table of its own rather than a flag column on each of the three
-- parents: the sidebar shows one list across the three kinds, in the order
-- they were pinned, which a column per table could only give back through
-- a three-way UNION on three timestamps; and a pin then goes with the row
-- it points at (`ON DELETE CASCADE`) without the album, artist or playlist
-- tables learning anything about the sidebar. Exactly one target per row.
CREATE TABLE sidebar_pin (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    album_id    INTEGER REFERENCES album(id)    ON DELETE CASCADE,
    artist_id   INTEGER REFERENCES artist(id)   ON DELETE CASCADE,
    playlist_id INTEGER REFERENCES playlist(id) ON DELETE CASCADE,
    pinned_at   INTEGER NOT NULL,
    CHECK ((album_id IS NOT NULL) + (artist_id IS NOT NULL) + (playlist_id IS NOT NULL) = 1)
);

-- One pin per target. Partial, so the NULLs of the other two kinds do not
-- collide (SQLite already treats NULLs as distinct; the WHERE keeps the
-- indexes to the rows they describe).
CREATE UNIQUE INDEX idx_sidebar_pin_album    ON sidebar_pin(album_id)    WHERE album_id    IS NOT NULL;
CREATE UNIQUE INDEX idx_sidebar_pin_artist   ON sidebar_pin(artist_id)   WHERE artist_id   IS NOT NULL;
CREATE UNIQUE INDEX idx_sidebar_pin_playlist ON sidebar_pin(playlist_id) WHERE playlist_id IS NOT NULL;
