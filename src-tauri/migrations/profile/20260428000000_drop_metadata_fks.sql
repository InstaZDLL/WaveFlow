-- Drop the orphan FK constraints on artist.deezer_id / album.deezer_id.
--
-- The previous metadata-cache migration (20260413000100) dropped the
-- `deezer_artist` and `deezer_album` tables when those caches moved to
-- `app.db`, but it left the FK declarations in place on the artist /
-- album columns. With the per-profile pool opening connections with
-- `foreign_keys=ON`, SQLite refuses to *prepare* any INSERT/UPDATE/DELETE
-- against those two tables because the referenced table no longer
-- exists in the same database file (cross-database FKs aren't a thing
-- in SQLite). The scanner then errors out on the very first
-- `upsert_artist`, before any track row is inserted, which surfaced as
-- "le scan ne capte aucune musique" on profiles created after the
-- previous migration ran.
--
-- The columns themselves stay (they still hold the Deezer ID used to
-- join `app.metadata_artist` / `app.metadata_album` for enrichment),
-- only the FK constraint is dropped via the standard SQLite
-- "create-new + copy + drop + rename" dance. Indexes and per-table
-- FTS triggers are reinstalled below.
--
-- `legacy_alter_table = ON` is required because the per-track FTS
-- triggers (`track_fts_insert`, `track_fts_delete`, `track_fts_update`)
-- reference `artist` / `album` in their bodies. Without legacy mode,
-- SQLite revalidates every trigger after each schema change in the
-- migration, and the brief window where `artist` is dropped but not
-- yet renamed back makes the validation fail. With legacy mode the
-- triggers are left untouched and resolve normally once the new
-- tables are in place under the original names.

PRAGMA legacy_alter_table = ON;

CREATE TABLE artist_new (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    canonical_name  TEXT NOT NULL,
    artwork_id      INTEGER REFERENCES artwork(id) ON DELETE SET NULL,
    deezer_id       INTEGER,
    UNIQUE (canonical_name)
);
INSERT INTO artist_new (id, name, canonical_name, artwork_id, deezer_id)
SELECT id, name, canonical_name, artwork_id, deezer_id FROM artist;
DROP TABLE artist;
ALTER TABLE artist_new RENAME TO artist;

CREATE INDEX idx_artist_deezer ON artist(deezer_id);

CREATE TRIGGER artist_name_fts_update AFTER UPDATE OF name ON artist BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, album_title, artist_name)
    SELECT 'delete', t.id,
           t.title,
           COALESCE((SELECT title FROM album WHERE id = t.album_id), ''),
           old.name
      FROM track t WHERE t.primary_artist = new.id;
    INSERT INTO track_fts (rowid, title, album_title, artist_name)
    SELECT t.id,
           t.title,
           COALESCE((SELECT title FROM album WHERE id = t.album_id), ''),
           new.name
      FROM track t WHERE t.primary_artist = new.id;
END;

CREATE TABLE album_new (
    id              INTEGER PRIMARY KEY,
    title           TEXT NOT NULL,
    canonical_title TEXT NOT NULL,
    artist_id       INTEGER REFERENCES artist(id) ON DELETE SET NULL,
    year            INTEGER,
    release_date    TEXT,
    total_tracks    INTEGER,
    total_discs     INTEGER,
    artwork_id      INTEGER REFERENCES artwork(id) ON DELETE SET NULL,
    deezer_id       INTEGER,
    UNIQUE (canonical_title, artist_id)
);
INSERT INTO album_new (
    id, title, canonical_title, artist_id, year, release_date,
    total_tracks, total_discs, artwork_id, deezer_id
)
SELECT
    id, title, canonical_title, artist_id, year, release_date,
    total_tracks, total_discs, artwork_id, deezer_id
FROM album;
DROP TABLE album;
ALTER TABLE album_new RENAME TO album;

CREATE INDEX idx_album_artist ON album(artist_id);

CREATE TRIGGER album_title_fts_update AFTER UPDATE OF title ON album BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, album_title, artist_name)
    SELECT 'delete', t.id,
           t.title,
           old.title,
           COALESCE((SELECT name FROM artist WHERE id = t.primary_artist), '')
      FROM track t WHERE t.album_id = new.id;
    INSERT INTO track_fts (rowid, title, album_title, artist_name)
    SELECT t.id,
           t.title,
           new.title,
           COALESCE((SELECT name FROM artist WHERE id = t.primary_artist), '')
      FROM track t WHERE t.album_id = new.id;
END;

PRAGMA legacy_alter_table = OFF;
