-- Listening history must outlive the tracks it points at (issue #367).
--
-- `play_event.track_id` was `NOT NULL ... ON DELETE CASCADE`, so removing a
-- folder (`DELETE FROM track WHERE folder_id = ?`) or a library silently
-- erased the matching history. A beta tester lost their stats five times
-- this way, and no backup helps: restoring the archive brings back the OLD
-- library, not the history plus a fresh scan.
--
-- Two changes:
--
--   1. `track_id` becomes nullable with `ON DELETE SET NULL`. Deleting a
--      track now orphans its history instead of destroying it.
--   2. Each event carries a snapshot of how to find its track again. The
--      re-attach pass at scan time walks them in priority order:
--        - `snapshot_hash`   the same bytes, moved or re-added
--        - `snapshot_path`   same file, tags rewritten (lofty rewrites the
--                            frames, so the blake3 changes while the path
--                            does not)
--        - artist + title    a re-rip or a different encoding entirely
--
-- SQLite cannot ALTER a column constraint, so this is the standard
-- rebuild-copy-swap. Existing rows are backfilled from the track they
-- currently point at, which is the only moment that information is
-- guaranteed to still be there.

CREATE TABLE play_event_new (
    id              INTEGER PRIMARY KEY,
    -- Nullable on purpose: an orphaned event is history waiting to be
    -- re-attached, not a broken row.
    track_id        INTEGER REFERENCES track(id) ON DELETE SET NULL,
    played_at       INTEGER NOT NULL,
    listened_ms     INTEGER NOT NULL,
    completed       INTEGER NOT NULL DEFAULT 0,
    skipped         INTEGER NOT NULL DEFAULT 0,
    source_type     TEXT,
    source_id       INTEGER,
    snapshot_hash   TEXT,
    snapshot_path   TEXT,
    snapshot_artist TEXT,
    snapshot_title  TEXT
);

INSERT INTO play_event_new
    (id, track_id, played_at, listened_ms, completed, skipped,
     source_type, source_id,
     snapshot_hash, snapshot_path, snapshot_artist, snapshot_title)
SELECT pe.id, pe.track_id, pe.played_at, pe.listened_ms, pe.completed,
       pe.skipped, pe.source_type, pe.source_id,
       t.file_hash, t.file_path, ar.name, t.title
  FROM play_event pe
  LEFT JOIN track  t  ON t.id  = pe.track_id
  LEFT JOIN artist ar ON ar.id = t.primary_artist;

DROP TABLE play_event;
ALTER TABLE play_event_new RENAME TO play_event;

CREATE INDEX idx_play_event_time  ON play_event(played_at DESC);
CREATE INDEX idx_play_event_track ON play_event(track_id, played_at DESC);

-- Drives the re-attach pass, which scans for orphans and matches them on
-- the snapshot columns. Partial so it stays tiny on a healthy library,
-- where almost every event is attached.
CREATE INDEX idx_play_event_orphan
    ON play_event(snapshot_hash, snapshot_path)
    WHERE track_id IS NULL;
