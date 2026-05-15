-- =============================================================================
-- Extend the lyrics.format CHECK constraint to accept the new 'ttml' value.
--
-- The original 20260413000000_metadata_caches.sql migration created the
-- table with `CHECK (format IN ('plain','lrc','enhanced_lrc'))`. SQLite has
-- no ALTER for CHECK constraints, so we rebuild the table: create a clone
-- with the broader CHECK, copy the rows, drop the original, rename.
--
-- No existing rows need transformation — both 'plain', 'lrc' and
-- 'enhanced_lrc' remain valid, and 'ttml' simply becomes a new accepted
-- value that the parser can now emit.
-- =============================================================================

CREATE TABLE lyrics_new (
    file_hash       TEXT PRIMARY KEY,
    content         TEXT NOT NULL,
    format          TEXT NOT NULL CHECK (format IN ('plain','lrc','enhanced_lrc','ttml')),
    source          TEXT NOT NULL CHECK (source IN ('embedded','lrc_file','api','manual')),
    language        TEXT,
    fetched_at      INTEGER NOT NULL
);

INSERT INTO lyrics_new (file_hash, content, format, source, language, fetched_at)
SELECT file_hash, content, format, source, language, fetched_at
FROM lyrics;

DROP TABLE lyrics;
ALTER TABLE lyrics_new RENAME TO lyrics;
