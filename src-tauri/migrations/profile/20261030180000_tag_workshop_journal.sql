-- The tag workshop's journal: every batch of files it wrote, and the
-- value each written field had before, so a batch can be undone.
--
-- New tables only. A file row outlives its track (`ON DELETE SET NULL`):
-- undoing a batch rewrites the file at `file_path`, and a track removed
-- from the library since must not take the record of what was written
-- into its file with it.

CREATE TABLE IF NOT EXISTS tag_workshop_batch (
    id            INTEGER PRIMARY KEY,
    created_at    INTEGER NOT NULL,
    -- What the batch was, in the user's words or the workshop's ("Nevermind
    -- — MusicBrainz"); shown in the list of batches.
    label         TEXT NOT NULL DEFAULT '',
    -- The release the batch's identifiers came from, when it was matched.
    release_mbid  TEXT,
    -- Set once every written file of the batch has been restored.
    undone_at     INTEGER
);

CREATE TABLE IF NOT EXISTS tag_workshop_file (
    id          INTEGER PRIMARY KEY,
    batch_id    INTEGER NOT NULL REFERENCES tag_workshop_batch(id) ON DELETE CASCADE,
    track_id    INTEGER REFERENCES track(id) ON DELETE SET NULL,
    file_path   TEXT NOT NULL,
    -- `pending`  journaled, the write not confirmed (an interrupted batch);
    -- `written`  written, `hash_after` is the file as the write left it;
    -- `failed`   the write was refused, the file is as it was;
    -- `undone`   the old values are back in the file.
    -- No CHECK: a constraint here could only be widened by rebuilding the
    -- table, and the values are the store's to keep straight.
    status      TEXT NOT NULL DEFAULT 'pending',
    -- BLAKE3 of the whole file after the write: a file whose bytes no
    -- longer hash to it was changed since, and an undo refuses it.
    hash_after  TEXT,
    error       TEXT
);

CREATE INDEX IF NOT EXISTS idx_tag_workshop_file_batch
    ON tag_workshop_file(batch_id);

CREATE TABLE IF NOT EXISTS tag_workshop_field (
    file_id    INTEGER NOT NULL REFERENCES tag_workshop_file(id) ON DELETE CASCADE,
    field      TEXT NOT NULL,
    -- NULL when the file did not carry the field.
    old_value  TEXT,
    new_value  TEXT NOT NULL,
    PRIMARY KEY (file_id, field)
);
