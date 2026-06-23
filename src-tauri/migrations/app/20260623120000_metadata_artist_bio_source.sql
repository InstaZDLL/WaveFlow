-- Track which source + language produced the cached artist bio so the
-- enrichment pass can invalidate it when the user switches bio source
-- (Last.fm / TheAudioDB) or the TheAudioDB language. Existing rows get
-- NULL and are treated as a cache miss for the bio on the next refresh,
-- so they re-fetch under the active source/language.
ALTER TABLE metadata_artist ADD COLUMN bio_source TEXT;
ALTER TABLE metadata_artist ADD COLUMN bio_language TEXT;
