-- The MusicBrainz id of an artist, as the audio files' own tags give it
-- (#1076).
--
-- `artist` is grouped by `canonical_name`, which ignores case: "LiSA" and
-- "LISA" are one row, and the display name went to whichever was written
-- last. Two files that carry *different* MusicBrainz artist ids are two
-- artists whatever their spelling, so the scanner now records the id it
-- read and keys a second identity under the same spelling apart. A file
-- with no id keeps being grouped by name alone.
--
-- Only ALTER TABLE ... ADD COLUMN: `artist` is a parent table
-- (track_artist, album and others reference it), and rebuilding it would
-- run its ON DELETE actions. `UNIQUE (canonical_name)` therefore stays; the
-- second artist of a spelling is keyed `<key> #<mbid>` by the scanner.
--
-- Existing rows stay NULL: a migration cannot read tags. The scanner fills
-- the id in as it meets the file, and a deep rescan relinks a library whose
-- namesakes were already merged.
ALTER TABLE artist ADD COLUMN mbid TEXT;

CREATE UNIQUE INDEX idx_artist_mbid ON artist(mbid) WHERE mbid IS NOT NULL;
