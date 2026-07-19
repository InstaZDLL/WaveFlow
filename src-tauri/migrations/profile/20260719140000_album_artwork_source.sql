-- Record where an album's cover came from, on the album itself (issue #401).
--
-- `artwork` rows are deduped on the content hash alone, and `source` is
-- only written on INSERT — so it records whoever put those bytes in the
-- library first, not where a given album got its cover. An image that is
-- embedded in one album's tags and shipped as a `cover.jpg` beside a
-- different one is a single row labelled `embedded`, and the sidecar
-- album then looks untouchable to `refresh_folder_covers`, whose
-- allowlist reads that column. Its cover silently stops refreshing.
--
-- Provenance is a property of the *link*, not of the bytes, so it moves
-- to the link.
--
-- Deliberately additive. Widening `artwork`'s uniqueness to
-- `(hash, source)` was the other candidate, but it needs the standard
-- create-copy-drop-rename rebuild, and `artwork` is a *parent* of
-- `album.artwork_id` / `artist.artwork_id` with `ON DELETE SET NULL`.
-- The profile pool opens connections with `foreign_keys = ON`, and
-- SQLite's DROP TABLE performs an implicit DELETE that fires foreign-key
-- actions — verified against a real database, in and out of a
-- transaction: the rebuild blanks every album and artist cover link in
-- the library. An ALTER TABLE ADD COLUMN cannot do that.
--
-- `artist` is untouched: its own guard is `artwork_id IS NULL`
-- (`link_local_artist_image`), which never reads `source`, so nothing
-- there depends on the column being accurate.

ALTER TABLE album ADD COLUMN artwork_source TEXT;

-- Backfill from the artwork row. This inherits the same ambiguity the
-- column exists to fix — a shared hash still reports whoever inserted it
-- first — but it is the best information available, and every write from
-- here on is unambiguous. A wrong `embedded` on an album whose cover is
-- really a sidecar self-corrects the first time the user replaces that
-- sidecar and a scan re-links it.
UPDATE album
   SET artwork_source = (
       SELECT aw.source FROM artwork aw WHERE aw.id = album.artwork_id
   )
 WHERE artwork_id IS NOT NULL;
