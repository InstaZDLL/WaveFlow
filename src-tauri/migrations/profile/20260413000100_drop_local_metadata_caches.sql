-- Move metadata caches to `app.db` so they're shared across profiles.
--
-- The new home is created by `migrations/app/20260413000000_metadata_caches.sql`.
-- The per-profile pool ATTACHes `app.db` as `app` on every connection, and
-- queries reference `app.deezer_artist`, `app.deezer_album`, `app.lyrics`.
--
-- Existing local cache rows are NOT migrated: re-fetching is cheap (Deezer
-- + Last.fm + LRCLIB are all fast and gracefully degrade) and the alternative
-- would require reading from one database while writing to another, which is
-- awkward in a SQL-only migration.
--
-- The `artist.deezer_id` and `album.deezer_id` columns keep their value but
-- their FK constraints become orphaned (the referenced tables no longer
-- exist locally). SQLite tolerates this when foreign_keys=ON because the
-- enforcement only fires on inserts/updates, and we never write a
-- deezer_id that points to a missing app.deezer_artist row.

DROP INDEX IF EXISTS idx_deezer_artist_expires;
DROP TABLE IF EXISTS deezer_artist;

DROP INDEX IF EXISTS idx_deezer_album_expires;
DROP TABLE IF EXISTS deezer_album;

DROP TABLE IF EXISTS lyrics;
