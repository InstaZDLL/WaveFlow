-- Custom tags the user's own files carry (#588).
--
-- Track lists shipped one fixed set of columns, so somebody whose
-- library is organised around a field WaveFlow does not model -- a
-- composer, a catalogue number, a rip source -- had no way to see it or
-- sort by it. A column for one of those needs the value stored, and the
-- value is not in `track`: these are exactly the frames the generic
-- lofty `Tag` cannot map onto an `ItemKey`.
--
-- One row per (track, key). A repeated key inside one file is a
-- multi-value field, and the scanner keeps the first: a column is one
-- cell, and joining the values would make a cell that is right for
-- nobody.
--
-- `ON DELETE CASCADE` so removing a track takes its tags with it -- the
-- rows describe a file, and they mean nothing once the file's row is
-- gone.
CREATE TABLE track_tag (
    track_id  INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    key       TEXT NOT NULL,
    value     TEXT NOT NULL,
    PRIMARY KEY (track_id, key)
) WITHOUT ROWID;

-- The picker asks "which keys exist, and on how many tracks" on every
-- open; the columns then ask for the values of two or three keys across
-- the whole library. Both are keyed on `key` first, so one index serves
-- them and neither has to walk the table.
CREATE INDEX idx_track_tag_key ON track_tag(key, track_id);
