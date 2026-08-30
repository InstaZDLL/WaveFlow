-- Offline copies of the bound server's tracks (lot 4 of the unified library).
--
-- A download is NOT a local library track. The managed folder it lives in is
-- deliberately invisible to the scanner, so no `track` row is ever created for
-- it -- which also means `remote_track_link` cannot describe it: that table
-- keys on `local_track_id REFERENCES track(id)`, and there is no such row. A
-- download describes a *remote* track that happens to be on this disk.
--
-- The link the plan calls "free" is a different one, and it is paid forward
-- rather than here: because the file is hashed while it is written, importing
-- it later into a scanned folder needs no re-read and no second hash, so the
-- exact reconciliation proof is already in hand the moment a `track` row
-- exists to attach it to.
--
-- No foreign key to `remote_track`: clearing the catalogue mirror drops
-- metadata, and it must not take the audio with it. An orphaned row points at
-- a file that still plays; re-mirroring restores the title around it.
CREATE TABLE remote_track_download (
    remote_track_id TEXT    PRIMARY KEY,
    -- Absolute path inside the profile's managed download directory.
    path            TEXT    NOT NULL,
    -- BLAKE3 of the whole file, computed while writing it. This is the same
    -- digest the server keys its own catalogue by, so it is directly
    -- comparable -- unlike the local library's `file_hash`, which covers a
    -- file the server has never seen.
    full_hash       TEXT    NOT NULL CHECK (length(full_hash) = 64),
    size            INTEGER NOT NULL CHECK (size > 0),
    downloaded_at   INTEGER NOT NULL
);

-- Answers "is this one already downloaded" for a batch of tracks, which is
-- what every listing asks before it can show a per-row state.
CREATE INDEX idx_remote_track_download_hash
    ON remote_track_download (full_hash);
