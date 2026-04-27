-- Rename the cached metadata tables to drop the misleading `deezer_*`
-- prefix.
--
-- These tables historically only stored Deezer responses, but they now
-- carry Last.fm artist bios as well (see `enrich_artist_deezer`'s 2nd
-- pass). The rows are still keyed by Deezer's stable artist/album IDs
-- — that's why the primary-key column keeps the `deezer_id` name —
-- but the table itself is the unified "metadata cache" surface and
-- should reflect that.
--
-- `ALTER TABLE ... RENAME TO ...` is the cheap path: it preserves all
-- rows and the column shapes intact, and SQLite has no foreign keys
-- pointing at these tables (cross-attached-database FKs aren't
-- enforced anyway, and the `artist.deezer_id` / `album.deezer_id`
-- columns in profile DBs are bare integers since the local
-- definitions were dropped in `profile/20260413000100_*`).
--
-- Indexes are renamed by drop + recreate because SQLite ALTER TABLE
-- doesn't follow them.

ALTER TABLE deezer_artist RENAME TO metadata_artist;
DROP INDEX IF EXISTS idx_deezer_artist_expires;
CREATE INDEX idx_metadata_artist_expires ON metadata_artist(expires_at);

ALTER TABLE deezer_album RENAME TO metadata_album;
DROP INDEX IF EXISTS idx_deezer_album_expires;
CREATE INDEX idx_metadata_album_expires ON metadata_album(expires_at);
