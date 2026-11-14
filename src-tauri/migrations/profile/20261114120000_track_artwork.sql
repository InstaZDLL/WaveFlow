-- A track's own cover (#1100).
--
-- A cover reached a track only through its album (`track.album_id` ->
-- `album.artwork_id`), so a file with no album tag showed none, whatever
-- picture it embedded: a video thumbnail kept by a downloader, typically.
-- The column holds the picture the scanner lifted from the file (or its
-- folder); the queries read the album's cover first and this one when the
-- album has none.
--
-- `ADD COLUMN`, never a rebuild of `track` or `artwork`: both are parents
-- of foreign keys, and `foreign_keys = ON` turns a `DROP TABLE` into a
-- cascading delete. `ON DELETE SET NULL` matches `album.artwork_id`.
--
-- Left NULL here: the scanner fills it on the next scan of each folder
-- (the `cover_backfill` pass), which re-reads the files a migration cannot.
ALTER TABLE track ADD COLUMN artwork_id INTEGER REFERENCES artwork(id) ON DELETE SET NULL;

CREATE INDEX idx_track_artwork ON track(artwork_id) WHERE artwork_id IS NOT NULL;
