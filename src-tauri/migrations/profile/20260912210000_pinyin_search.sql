-- Reach a Chinese title by typing pinyin, or its initials (#579).
--
-- The substring half of that issue shipped in the trigram rebuild
-- (20260910090000): `人民` now finds `中国人民解放军`. What it does not
-- give is the thing the request actually asked for -- typing `zhongguo`
-- or `zgr` on a Latin keyboard, without switching input method.
--
-- Pinyin cannot be derived in SQL, so it is computed in Rust
-- (`scanner::canonical::pinyin_blob`) and stored beside the `canonical_*`
-- columns it is maintained with: wherever one is written, so is the
-- other. Each column holds the syllables, a space, then the initials --
-- `中国人` becomes `zhongguoren zgr` -- so one match answers both forms.
-- The space is load-bearing: the search planner splits terms on
-- whitespace, so no single term can straddle it.
--
-- NULL means "not computed yet" and the empty string means "computed,
-- nothing to romanise". The backfill needs that distinction to
-- terminate: most libraries are entirely Latin, and without it every
-- launch would re-scan every row forever.

ALTER TABLE track  ADD COLUMN pinyin TEXT;
ALTER TABLE album  ADD COLUMN pinyin TEXT;
ALTER TABLE artist ADD COLUMN pinyin TEXT;

-- No index on these columns, deliberately: the queries that read them
-- are `LIKE '%…%'`, which no b-tree can serve, and the fast route goes
-- through the FTS index below instead.

-- `track_fts` gains a fourth column carrying all three blobs, so one
-- MATCH covers text and pinyin alike and the index keeps its ranking.
-- The triggers only ever COPY what Rust computed, which is why the
-- feature can live in an index that cannot transliterate anything.
--
-- Rebuilt rather than altered: an FTS5 table cannot gain a column
-- through ALTER. Dropping it is safe for the reason the previous rebuild
-- gives -- it is a virtual table with no foreign keys pointing at it --
-- and its triggers are dropped first because they reference it.

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
    pinyin,
    tokenize='trigram remove_diacritics 1'
);

INSERT INTO track_fts (rowid, title, album_title, artist_name, pinyin)
SELECT t.id,
       t.title,
       COALESCE(al.title, ''),
       COALESCE(ar.name, ''),
       -- Empty on the way in: the columns above were added by this same
       -- migration, so every row is NULL until the backfill has run.
       COALESCE(t.pinyin, '') || ' ' || COALESCE(al.pinyin, '') || ' '
           || COALESCE(ar.pinyin, '')
  FROM track t
  LEFT JOIN album  al ON al.id = t.album_id
  LEFT JOIN artist ar ON ar.id = t.primary_artist;

-- Keep track_fts in sync with track -----------------------------------------

CREATE TRIGGER track_fts_insert AFTER INSERT ON track BEGIN
    INSERT INTO track_fts (rowid, title, album_title, artist_name, pinyin) VALUES (
        new.id,
        new.title,
        COALESCE((SELECT title FROM album  WHERE id = new.album_id),       ''),
        COALESCE((SELECT name  FROM artist WHERE id = new.primary_artist), ''),
        COALESCE(new.pinyin, '') || ' '
            || COALESCE((SELECT pinyin FROM album  WHERE id = new.album_id),       '') || ' '
            || COALESCE((SELECT pinyin FROM artist WHERE id = new.primary_artist), '')
    );
END;

CREATE TRIGGER track_fts_delete AFTER DELETE ON track BEGIN
    DELETE FROM track_fts WHERE rowid = old.id;
END;

-- `pinyin` joins the watched columns: the backfill writes it long after
-- the row was inserted, and an index that never heard about it would
-- leave the feature working only for tracks scanned afterwards.
CREATE TRIGGER track_fts_update
AFTER UPDATE OF title, album_id, primary_artist, pinyin ON track BEGIN
    UPDATE track_fts SET
        title        = new.title,
        album_title  = COALESCE((SELECT title FROM album  WHERE id = new.album_id),       ''),
        artist_name  = COALESCE((SELECT name  FROM artist WHERE id = new.primary_artist), ''),
        pinyin       = COALESCE(new.pinyin, '') || ' '
            || COALESCE((SELECT pinyin FROM album  WHERE id = new.album_id),       '') || ' '
            || COALESCE((SELECT pinyin FROM artist WHERE id = new.primary_artist), '')
      WHERE rowid = new.id;
END;

-- A rename on either side has to reach every track that points at it --
-- and so does a pinyin backfill, which is why both columns are watched.
-- The whole blob is recomputed rather than patched in place: it is the
-- concatenation of three sources, and rebuilding it from the row being
-- updated is the only version that cannot drift.

CREATE TRIGGER artist_name_fts_update AFTER UPDATE OF name, pinyin ON artist BEGIN
    UPDATE track_fts SET
        artist_name = new.name,
        pinyin = (
            SELECT COALESCE(t.pinyin, '') || ' ' || COALESCE(al.pinyin, '') || ' '
                       || COALESCE(ar.pinyin, '')
              FROM track t
              LEFT JOIN album  al ON al.id = t.album_id
              LEFT JOIN artist ar ON ar.id = t.primary_artist
             WHERE t.id = track_fts.rowid
        )
     WHERE rowid IN (SELECT id FROM track WHERE primary_artist = new.id);
END;

CREATE TRIGGER album_title_fts_update AFTER UPDATE OF title, pinyin ON album BEGIN
    UPDATE track_fts SET
        album_title = new.title,
        pinyin = (
            SELECT COALESCE(t.pinyin, '') || ' ' || COALESCE(al.pinyin, '') || ' '
                       || COALESCE(ar.pinyin, '')
              FROM track t
              LEFT JOIN album  al ON al.id = t.album_id
              LEFT JOIN artist ar ON ar.id = t.primary_artist
             WHERE t.id = track_fts.rowid
        )
     WHERE rowid IN (SELECT id FROM track WHERE album_id = new.id);
END;
