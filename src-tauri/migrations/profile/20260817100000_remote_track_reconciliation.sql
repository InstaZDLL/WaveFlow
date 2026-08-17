-- M5: conservative local/server reconciliation.
--
-- A link belongs to this profile database. The server cannot persist it:
-- only the desktop knows the local row id and file path. The remote side has
-- no foreign key on purpose because `remote_track` is a disposable cache;
-- dropping or rebuilding that cache must not destroy a confirmed link.

CREATE TABLE remote_track_link (
    local_track_id       INTEGER PRIMARY KEY
                                 REFERENCES track(id) ON DELETE CASCADE,
    remote_track_id      TEXT    NOT NULL UNIQUE,
    method               TEXT    NOT NULL
                                 CHECK (method IN ('exact_full_hash', 'confirmed_mbid')),
    verified_full_hash   TEXT,
    status               TEXT    NOT NULL
                                 CHECK (status IN ('confirmed', 'stale')),
    playback_preference  TEXT    NOT NULL DEFAULT 'local_first'
                                 CHECK (playback_preference IN ('local_first', 'server_first')),
    confirmed_at         INTEGER NOT NULL,
    verified_at          INTEGER NOT NULL,
    -- An exact-hash link must carry its 64-char proof. `length(NULL) = 64`
    -- evaluates to NULL (not FALSE), so a bare length check would let a NULL
    -- proof slip through — require non-NULL explicitly.
    CHECK (method != 'exact_full_hash'
           OR (verified_full_hash IS NOT NULL AND length(verified_full_hash) = 64))
);

CREATE INDEX idx_remote_track_link_status ON remote_track_link(status);

-- The reconciliation prefilter joins local files to remote tracks on byte
-- size (then verifies with a full hash), so index the remote side's size.
CREATE INDEX idx_remote_track_size ON remote_track(size);

-- A rejected pair stays hidden while the proof that produced it is unchanged.
-- It is separate from `remote_track_link`: a rejected candidate is explicitly
-- not an identity link and duplicate groups may reject several pairs.
CREATE TABLE remote_track_match_rejection (
    local_track_id   INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    remote_track_id  TEXT    NOT NULL,
    proof_kind       TEXT    NOT NULL CHECK (proof_kind IN ('exact_full_hash', 'mbid')),
    proof            TEXT    NOT NULL,
    rejected_at      INTEGER NOT NULL,
    PRIMARY KEY (local_track_id, remote_track_id, proof_kind, proof)
);
