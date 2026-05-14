-- =============================================================================
-- album_artist + is_compilation
-- =============================================================================
--
-- The scanner used to identify albums by `(canonical_title, primary_artist of
-- first track)`. That key splits any album whose tracks list different lead
-- performers — featurings ("Main, Guest" on one track, "Main" on the next),
-- compilation albums where every track has a different artist, etc. The fix
-- is to honour the Album Artist tag (TPE2 / aART / ALBUMARTIST / "Album
-- Artist") when present and the Compilation flag (TCMP / cpil / COMPILATION /
-- 1) for actual various-artists records.
--
-- - `album_artist` stores the raw text exactly as written in the source tag,
--   preserving casing for the UI. Resolved to an `artist` row in parallel via
--   the existing `album.artist_id` FK, which now points to the album artist
--   (was: the first track's primary artist). When the tag is absent, the
--   scanner falls back to the previous behaviour so untouched files keep
--   their existing grouping.
-- - `is_compilation = 1` triggers the synthetic "Various Artists" artist row,
--   so a compilation tagged with TCMP merges its tracks under a single album
--   even when no Album Artist tag is set.
--
-- Existing rows are left with both columns at NULL / 0; the next scan
-- (manual rescan, fs-watcher event or scan-on-start) walks every file and
-- backfills these values from the source tags. No data is destroyed by the
-- migration itself.

ALTER TABLE album ADD COLUMN album_artist TEXT;
ALTER TABLE album ADD COLUMN is_compilation INTEGER NOT NULL DEFAULT 0;
