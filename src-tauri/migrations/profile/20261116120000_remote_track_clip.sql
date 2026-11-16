-- Whether a server track has a music video, as the server says it.
--
-- The server links a track to the video file named like it (its RFC-029),
-- by the rule this app follows locally, and says so on every track it
-- describes:
-- `"clip": {"format": "mp4", "byte_size": …}` or `"clip": null`. Kept here so
-- a menu can offer the clip, offline included, without asking the server
-- about each row; the clip itself is never stored, and playing it mints a
-- ticket each time (`remote::clip`).
--
-- `has_clip` has three states, because the server has three answers:
--
-- - `1`: the server named a clip;
-- - `0`: it said `null`, so there is none, and a clip that went away is
--   cleared by that `null`;
-- - `NULL`: it said nothing, being older than the field or another kind of
--   server. Nothing is known, and a later sighting that says nothing keeps
--   whatever an earlier one said.
--
-- The format and size are the server's words, kept for display, and only
-- beside `has_clip = 1`.
--
-- Written by `projection::cache_song`, the single place a remote track row
-- is written.
ALTER TABLE remote_track ADD COLUMN has_clip INTEGER CHECK (has_clip IN (0, 1));
ALTER TABLE remote_track ADD COLUMN clip_format TEXT;
ALTER TABLE remote_track ADD COLUMN clip_byte_size INTEGER;

-- Existing rows say nothing, and most would go on saying nothing: the walk
-- skips an album whose `song_count` is unchanged, so `cache_song` never runs
-- again for its tracks, and a clip that was already on the server when this
-- version arrived would never be offered. Clearing the stamps makes every
-- album stale once; the next walk re-fetches it and fills the columns, and
-- it is incremental again afterwards (as in 20260826180000).
UPDATE remote_album SET mirrored_at = NULL;
