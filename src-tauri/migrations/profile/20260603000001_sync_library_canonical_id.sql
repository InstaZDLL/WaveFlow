-- Canonical entity ids for the `library` table. Phase 1.f.desktop.5.
--
-- Same shape as the playlist migration in 20260603000000: add a
-- nullable `canonical_id TEXT` column, backfill every existing row
-- with a fresh UUIDv4, plant a UNIQUE index, install AFTER INSERT +
-- BEFORE UPDATE triggers so the column behaves as NOT NULL UNIQUE
-- without an ALTER COLUMN (unsupported in SQLite).
--
-- Track / liked_track do NOT need their own canonical column or
-- mapping row: `track.file_hash` (BLAKE3) is already a stable,
-- cross-device identifier (same file content → same hash on every
-- install), so liked / rating ops carry the file_hash as
-- `entity_id` directly. Library, in contrast, is user-curated and
-- has no natural cross-device key, so it follows the playlist
-- pattern.

ALTER TABLE library ADD COLUMN canonical_id TEXT;

WITH src AS (
    SELECT id, lower(hex(randomblob(16))) AS h
      FROM library
)
UPDATE library
   SET canonical_id = (
       SELECT substr(s.h,  1, 8)              || '-'
           || substr(s.h,  9, 4)              || '-'
           || '4' || substr(s.h, 14, 3)       || '-'
           || substr('89ab', (random() & 3) + 1, 1)
                  || substr(s.h, 18, 3)       || '-'
           || substr(s.h, 21, 12)
         FROM src s
        WHERE s.id = library.id
   )
 WHERE canonical_id IS NULL;

CREATE UNIQUE INDEX idx_library_canonical_id ON library(canonical_id);

-- Runtime invariant: identical setup to playlist (see the matching
-- migration's comment for the design rationale — BEFORE INSERT
-- triggers can't mutate NEW in SQLite, so we use AFTER INSERT to
-- fill in the column when missing).

CREATE TRIGGER trg_library_set_canonical_id_on_insert
AFTER INSERT ON library
FOR EACH ROW
WHEN NEW.canonical_id IS NULL
BEGIN
    UPDATE library
       SET canonical_id = (
           WITH s(h) AS (SELECT lower(hex(randomblob(16))))
           SELECT substr(s.h,  1, 8)              || '-'
               || substr(s.h,  9, 4)              || '-'
               || '4' || substr(s.h, 14, 3)       || '-'
               || substr('89ab', (random() & 3) + 1, 1)
                      || substr(s.h, 18, 3)       || '-'
               || substr(s.h, 21, 12)
             FROM s
       )
     WHERE id = NEW.id AND canonical_id IS NULL;
END;

CREATE TRIGGER trg_library_prevent_null_canonical_id_on_update
BEFORE UPDATE OF canonical_id ON library
FOR EACH ROW
WHEN NEW.canonical_id IS NULL
BEGIN
    SELECT RAISE(ABORT, 'library.canonical_id may not be NULL');
END;

-- Seed sync_id_map for every existing library so inbound ops the
-- WS subscriber receives can resolve immediately without an
-- ensure-on-miss roundtrip.
INSERT INTO sync_id_map (entity, canonical_id, local_id)
SELECT 'library', canonical_id, id
  FROM library
 WHERE canonical_id IS NOT NULL;
