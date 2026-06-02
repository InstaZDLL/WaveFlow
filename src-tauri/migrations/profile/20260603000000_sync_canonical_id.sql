-- Canonical entity ids for cross-device sync. Phase 1.f.desktop.4b
-- of RFC-001 §6.6.
--
-- The sync protocol up to 1.f.desktop.4a (#196) sent the local
-- `playlist.id INTEGER` as `entity_id` in every outbound op. That is
-- fine for the push direction — the server keys its UNIQUEs on
-- `(user_id, device_id, entity, entity_id)`, so different devices'
-- ops live in disjoint namespaces and can't collide. It does NOT
-- cross devices cleanly: device A's `playlist#42` and device B's
-- `playlist#42` are not the same playlist, so a fan-in subscriber
-- on device B can't blindly look up `entity_id=42` against its own
-- `playlist` table.
--
-- This migration introduces a stable per-entity UUIDv4 the desktop
-- assigns at insert (and backfills for every pre-existing playlist
-- row at migration time). Every outbound op now carries the
-- canonical id; every inbound op goes through a mapping table that
-- translates back to the local rowid.
--
-- ## Scope
--
-- Today this migration only covers the `playlist` table — the only
-- syncable entity with outbound hooks wired in 1.f.desktop.2b. When
-- `library` / `track` / `liked_track` follow in later sub-PRs, each
-- gets its own ALTER + backfill in a dated migration, and
-- `sync_id_map.entity` grows the matching string.

ALTER TABLE playlist ADD COLUMN canonical_id TEXT;

-- Backfill: assign every existing playlist a fresh UUIDv4 so the
-- column reaches the NOT-NULL invariant the application code relies
-- on. SQLite has no native UUID generator; we synthesise an RFC-4122
-- v4 string from `randomblob(16)` then patch the version + variant
-- nibbles to keep round-trips through `uuid::Uuid::parse_str` clean.
--
-- The hex layout the substr() chain builds is
-- `xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx` where:
--   - the `4` at the start of the third group is the version nibble
--   - the `y` at the start of the fourth group is one of {8,9,a,b}
--     (the variant nibble — we pick from `'89ab'` via `random() & 3`,
--     which extracts the bottom two bits and is overflow-safe; the
--     alternative `abs(random()) % 4` blows up on `random() ==
--     INT64_MIN` because abs(INT64_MIN) is unrepresentable in i64
--     and SQLite raises "integer overflow")
WITH src AS (
    SELECT id, lower(hex(randomblob(16))) AS h
      FROM playlist
)
UPDATE playlist
   SET canonical_id = (
       SELECT substr(s.h,  1, 8)              || '-'
           || substr(s.h,  9, 4)              || '-'
           || '4' || substr(s.h, 14, 3)       || '-'
           || substr('89ab', (random() & 3) + 1, 1)
                  || substr(s.h, 18, 3)       || '-'
           || substr(s.h, 21, 12)
         FROM src s
        WHERE s.id = playlist.id
   )
 WHERE canonical_id IS NULL;

CREATE UNIQUE INDEX idx_playlist_canonical_id ON playlist(canonical_id);

-- Runtime enforcement of the non-null invariant. SQLite's UNIQUE
-- index allows multiple NULL rows, and adding `NOT NULL` to an
-- existing column requires the rebuild-table dance — these triggers
-- are a lower-friction backstop that the application layer's
-- `ensure_local_playlist` would have to bypass to plant a NULL.
--
-- The AFTER INSERT trigger fills in a fresh UUIDv4 when a row lands
-- without one (SQLite BEFORE INSERT triggers can't mutate NEW — the
-- canonical workaround is an AFTER INSERT that UPDATEs the row).
-- The BEFORE UPDATE trigger RAISEs if a later UPDATE tries to NULL
-- the column back out.
--
-- Together with `idx_playlist_canonical_id` (which enforces
-- uniqueness on the non-null values), the table behaves like
-- `canonical_id TEXT NOT NULL UNIQUE` without an ALTER COLUMN
-- (unsupported in SQLite). The AFTER INSERT UPDATE itself does not
-- fire the BEFORE UPDATE guard because it sets a non-NULL value.

CREATE TRIGGER trg_playlist_set_canonical_id_on_insert
AFTER INSERT ON playlist
FOR EACH ROW
WHEN NEW.canonical_id IS NULL
BEGIN
    UPDATE playlist
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

CREATE TRIGGER trg_playlist_prevent_null_canonical_id_on_update
BEFORE UPDATE OF canonical_id ON playlist
FOR EACH ROW
WHEN NEW.canonical_id IS NULL
BEGIN
    SELECT RAISE(ABORT, 'playlist.canonical_id may not be NULL');
END;

-- Local ↔ canonical id mapping for INBOUND ops the WS subscriber
-- applies. Two roles:
--
-- 1. Resolve an inbound `entity_id` (a canonical UUID minted on
--    another device) to the local rowid this desktop's tables key on.
--    A miss means the entity doesn't exist locally yet — the apply
--    path creates it and inserts the mapping row in the same tx.
-- 2. Survive local rowid reuse. SQLite reuses rowids after deletes
--    (unless the table is AUTOINCREMENT, which `playlist` isn't);
--    routing through the mapping keeps the desktop's view stable
--    even if a future schema change drops + re-creates the local id.
--
-- `entity` is free-form to mirror `sync_pending_op.entity`. Adding a
-- new entity type (library, track, …) just appends a row family —
-- no schema change here.
CREATE TABLE sync_id_map (
    entity        TEXT NOT NULL,
    canonical_id  TEXT NOT NULL,
    local_id      INTEGER NOT NULL,
    PRIMARY KEY (entity, canonical_id)
);

-- Reverse lookup the apply path uses after a local INSERT lands
-- and the caller needs to map back from `local_id` to a canonical
-- already broadcast by another device.
CREATE INDEX idx_sync_id_map_local
    ON sync_id_map (entity, local_id);

-- Seed the mapping table with the playlists we just backfilled so
-- the desktop can resolve inbound ops against pre-1.f rows without
-- a hand-import step. A future device joining the same account will
-- get those rows via the catch-up GET /api/v1/sync/ops pass; that
-- happens AFTER this seed so the local rows are already
-- mapping-resolvable when the first remote op lands.
INSERT INTO sync_id_map (entity, canonical_id, local_id)
SELECT 'playlist', canonical_id, id
  FROM playlist
 WHERE canonical_id IS NOT NULL;

-- High-water mark for the catch-up REST pass. The WS subscriber
-- advances this after every successfully-applied op (whether the op
-- arrived via WS or via the GET /sync/ops pull). On reconnect the
-- pull resumes from `since = sync.last_seen_id` so we don't replay
-- the entire log every restart — and the server's 410 Gone guard
-- (compaction watermark) kicks in when the value has fallen too far
-- behind.
--
-- Per-profile because it's tied to the JWT's Better Auth user. App-
-- wide would risk one profile's catch-up leaking ops to another
-- after a profile switch.
INSERT INTO profile_setting (key, value, value_type, updated_at)
VALUES ('sync.last_seen_id', '0', 'int', strftime('%s','now') * 1000)
ON CONFLICT(key) DO NOTHING;
