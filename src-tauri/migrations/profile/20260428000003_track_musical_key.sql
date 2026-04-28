-- Capture the tagged musical key per track. ID3v2 calls it `TKEY`,
-- Vorbis comments / MP4 atoms / APE / WavPack call it `INITIALKEY`;
-- DJ tooling (Mixed In Key, Rekordbox, Traktor, Serato) writes
-- 1-3 character codes like `Am`, `F#`, `Abm`, plus the Camelot/Open
-- Key wheel notation as a fallback (`8A`, `12B`).
--
-- Stored on `track` rather than `track_analysis` because it's a tag
-- read at scan time, not a value computed by the audio analyzer.
ALTER TABLE track ADD COLUMN musical_key TEXT;
