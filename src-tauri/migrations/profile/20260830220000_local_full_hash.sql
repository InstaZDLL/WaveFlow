-- A cache of the whole-file digest of local tracks.
--
-- `track.file_hash` is deliberately a partial digest — head and tail — so a
-- rescan does not read the whole library. The server's `full_hash` covers the
-- entire file, and the two are incompatible by construction, which is why
-- reconciliation reads candidate files in full and why uploading "everything
-- the server is missing" would otherwise read the whole library on every pass.
--
-- Reading it once is acceptable. Reading it once per pass is not, so the
-- result is kept here.
--
-- The entry is valid only while `(file_size, file_modified)` still match the
-- row on `track` — the same pair the scanner's fast path trusts to decide a
-- file has not changed. A retag moves at least one of them in the ordinary
-- case, which invalidates the digest rather than leaving a stale proof that
-- would be handed to a server as an identity.
--
-- That pair is not enough on its own, and this is where this table asks for
-- more than the scanner does. A tool that rewrites a file to the same size
-- while preserving its mtime defeats it (issue #366, symptom A), and a deep
-- rescan does not repair it either: `file_modified` is read from the file, so
-- a preserved mtime stays preserved. The scanner can live with that — it
-- shows a stale tag until someone rescans. A stale digest offered to a server
-- as "these are the bytes I hold" is a different order of wrong, so the one
-- path that knows a file changed without waiting to be told — this app's own
-- tag writer — drops the entry explicitly.
CREATE TABLE local_full_hash (
    track_id      INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    full_hash     TEXT    NOT NULL CHECK (length(full_hash) = 64),
    file_size     INTEGER NOT NULL,
    file_modified INTEGER NOT NULL,
    computed_at   INTEGER NOT NULL
);

-- Answering "does the server already hold these bytes?" is a lookup by digest
-- across the whole table, not by track.
CREATE INDEX idx_local_full_hash_digest ON local_full_hash (full_hash);
