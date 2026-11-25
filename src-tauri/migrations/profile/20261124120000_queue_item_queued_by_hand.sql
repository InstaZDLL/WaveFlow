-- Which queue rows the listener put there by hand (#1168).
--
-- "Add to queue" and "Play next" write `source_type = 'manual'`, but so
-- does every fill that has no better source to name: a selection, the
-- history, the Release Radar, files opened from the system. The source
-- therefore cannot tell the tracks a listener queued from the list they
-- started, and a fresh fill has to keep the first and replace the
-- second. This column is that answer.
--
-- No backfill: a row written before this migration cannot say which of
-- the two it was, and reading every `'manual'` row as queued by hand
-- would pin a whole played selection ahead of the next album. Rows from
-- before simply behave as they did until they are played or replaced.
ALTER TABLE queue_item
    ADD COLUMN queued_by_hand INTEGER NOT NULL DEFAULT 0
        CHECK (queued_by_hand IN (0, 1));
