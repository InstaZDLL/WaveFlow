-- The remote track cache now keeps the artist's server id alongside its
-- name, so a projected track can navigate to a remote artist view. The
-- server started exposing `SongItem.artist_id` (RFC-005); `album_id` was
-- already cached. Additive ALTER — never redefine a merged migration.
ALTER TABLE remote_track ADD COLUMN artist_id TEXT;
