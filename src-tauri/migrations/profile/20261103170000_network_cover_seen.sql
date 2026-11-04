-- The sidecar cover a share's listing showed when an album last took stock of
-- it. `artwork.hash` is the baseline on a local folder, where hashing costs a
-- disk read; on a share it would mean downloading every cover on every scan.
-- A listing already gives the name, size and modification time, so a rescan
-- compares those and fetches an image only when they moved.
--
-- A side table rather than columns on `album`: only an album on a share has a
-- row. The row goes with its album, and losing one costs one image read.
CREATE TABLE network_cover_seen (
    album_id       INTEGER PRIMARY KEY REFERENCES album(id) ON DELETE CASCADE,
    file_name      TEXT NOT NULL,
    file_size      INTEGER NOT NULL,
    file_modified  INTEGER NOT NULL
);
