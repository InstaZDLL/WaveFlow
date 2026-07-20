-- Manual motion-cover override for an album (issue #408). A user-supplied
-- mp4 always wins over a plugin-resolved motion cover and survives plugin
-- re-fetches, the same precedence rule `album.artwork_source = 'manual'`
-- gives static covers (see 20260719140000_album_artwork_source.sql). The
-- row's existence is itself the "manual" signal -- nothing else writes
-- this table, so there is no separate provenance column to guard.
--
-- A fresh CREATE TABLE, not an ALTER on `album`/`artwork`, so it carries
-- none of the DROP TABLE / foreign-key cascade risk documented in
-- CLAUDE.md for those two tables.
CREATE TABLE album_motion_artwork (
    album_id   INTEGER PRIMARY KEY REFERENCES album(id) ON DELETE CASCADE,
    hash       TEXT NOT NULL,
    format     TEXT NOT NULL DEFAULT 'mp4',
    created_at INTEGER NOT NULL
);
