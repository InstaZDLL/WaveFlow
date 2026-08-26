-- Sort keys for the mirrored tracks, for the same reason the albums have them.
--
-- The unified track listing sorts both halves against each other, and the
-- local half sorts artist and album on `artist.canonical_name` /
-- `album.canonical_title` — forms produced by
-- `waveflow_core::metadata::name_match::normalize_name`, which lowercases,
-- folds diacritics and drops punctuation. SQLite reproduces none of that, so
-- a remote half sorted on its raw display strings interleaves wrongly and
-- splits an artist in two down the middle of the list.
--
-- Only artist and album. The title has no canonical form on the local side
-- either — it sorts on `track.title COLLATE NOCASE` — so both halves sort the
-- title on their display string, which keeps that column consistent by using
-- the same expression rather than by normalising one side only.
--
-- Filled by `projection::cache_song`, the single place a remote track row is
-- written: from the snapshot, from a change event, from a search hit and from
-- the catalogue walk alike. Nullable because rows cached before this migration
-- have none; the listing falls back to the display string, and any later
-- sighting of the track fills them in.
ALTER TABLE remote_track ADD COLUMN sort_artist TEXT;
ALTER TABLE remote_track ADD COLUMN sort_album TEXT;

CREATE INDEX idx_remote_track_sort ON remote_track (sort_artist, sort_album, title);
