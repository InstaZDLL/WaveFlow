-- A backdrop the user chose for an artist — issue #693.
--
-- The square photo already had `artist.artwork_id`; the wide hero behind
-- the artist header had nothing, so whatever TheAudioDB listed first was
-- final. This is its pair: the artwork row it points at lives in the
-- profile artwork dir like every other manual pick, so it survives a
-- metadata cache prune, and it takes precedence over the shared
-- `app.metadata_artist.background_hash` wherever the hero is resolved.
--
-- NULL means "no choice made", which is the automatic behaviour. A
-- deliberate "no backdrop at all" is not this column: that is the
-- per-profile `ui.artist_hero` toggle, which already exists.
--
-- ON DELETE SET NULL for the reason the column is nullable at all: an
-- artwork row swept by a future GC must not take the artist row with it.
ALTER TABLE artist ADD COLUMN background_artwork_id INTEGER
  REFERENCES artwork(id) ON DELETE SET NULL;
