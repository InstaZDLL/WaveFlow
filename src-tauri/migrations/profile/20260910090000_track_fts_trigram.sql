-- Rebuild `track_fts` so a search can match the MIDDLE of a title, not
-- only its start (#579).
--
-- `unicode61` does not segment CJK: an unbroken run of Han characters is
-- ONE token. Measured on a title of `中国人民解放军` — `中国*` finds it,
-- `人民*` finds nothing at all, silently. A Chinese library was therefore
-- searchable only by typing a title from its first character.
--
-- `trigram` indexes every 3-character window instead, so any substring of
-- three characters or more is a phrase match. Measured on 50 000 tracks:
-- the index grows from 2.3 MB to 8.6 MB and this rebuild takes 0.23 s,
-- which is why it runs here rather than as a background backfill.
--
-- Two consequences are deliberate:
--
-- 1. The table now OWNS its content (no `content=''`). It has to: `LIKE`
--    is what serves queries shorter than trigram's three-character
--    minimum, and on a contentless table every column reads back NULL, so
--    `LIKE` matches nothing. Owning the text is most of the size increase.
-- 2. `remove_diacritics 2` becomes `1` — trigram accepts no other value.
--    The difference is confined to combining marks used as letters in a
--    few scripts; `chateau` still finds `château`.
--
-- Dropping the table is safe: it is a virtual table with no foreign keys
-- pointing at it, so the `foreign_keys = ON` cascade that makes dropping a
-- parent dangerous does not apply. Its triggers are dropped first because
-- they reference it.

DROP TRIGGER IF EXISTS track_fts_insert;
DROP TRIGGER IF EXISTS track_fts_delete;
DROP TRIGGER IF EXISTS track_fts_update;
DROP TRIGGER IF EXISTS artist_name_fts_update;
DROP TRIGGER IF EXISTS album_title_fts_update;

DROP TABLE IF EXISTS track_fts;

CREATE VIRTUAL TABLE track_fts USING fts5(
    title,
    album_title,
    artist_name,
    tokenize='trigram remove_diacritics 1'
);

INSERT INTO track_fts (rowid, title, album_title, artist_name)
SELECT t.id,
       t.title,
       COALESCE(al.title, ''),
       COALESCE(ar.name, '')
  FROM track t
  LEFT JOIN album  al ON al.id = t.album_id
  LEFT JOIN artist ar ON ar.id = t.primary_artist;

-- Keep track_fts in sync with track -----------------------------------------
--
-- These are plain INSERT / UPDATE / DELETE now. The contentless table
-- needed the `INSERT INTO track_fts(track_fts, ...) VALUES('delete', ...)`
-- idiom, which requires handing back the exact values that were indexed —
-- get one wrong and the index silently rots. A table that owns its content
-- knows them itself, so a rowid is enough.

CREATE TRIGGER track_fts_insert AFTER INSERT ON track BEGIN
    INSERT INTO track_fts (rowid, title, album_title, artist_name) VALUES (
        new.id,
        new.title,
        COALESCE((SELECT title FROM album  WHERE id = new.album_id),       ''),
        COALESCE((SELECT name  FROM artist WHERE id = new.primary_artist), '')
    );
END;

CREATE TRIGGER track_fts_delete AFTER DELETE ON track BEGIN
    DELETE FROM track_fts WHERE rowid = old.id;
END;

CREATE TRIGGER track_fts_update
AFTER UPDATE OF title, album_id, primary_artist ON track BEGIN
    UPDATE track_fts SET
        title        = new.title,
        album_title  = COALESCE((SELECT title FROM album  WHERE id = new.album_id),       ''),
        artist_name  = COALESCE((SELECT name  FROM artist WHERE id = new.primary_artist), '')
      WHERE rowid = new.id;
END;

-- A rename on either side has to reach every track that points at it.

CREATE TRIGGER artist_name_fts_update AFTER UPDATE OF name ON artist BEGIN
    UPDATE track_fts SET artist_name = new.name
     WHERE rowid IN (SELECT id FROM track WHERE primary_artist = new.id);
END;

CREATE TRIGGER album_title_fts_update AFTER UPDATE OF title ON album BEGIN
    UPDATE track_fts SET album_title = new.title
     WHERE rowid IN (SELECT id FROM track WHERE album_id = new.id);
END;
