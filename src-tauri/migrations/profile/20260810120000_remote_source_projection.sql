-- Remote music source + user-data sync v2 projection. RFC-005.
--
-- The server is authoritative over its own catalogue, and this protocol
-- synchronizes only the state the account owns: playlists, favourites,
-- ratings, history, queue and shares. Those all reference the SERVER's
-- tracks, which have no local counterpart — matching a local file to a
-- server track is deliberately out of scope (RFC-005 §Decision 1).
--
-- That is why none of this lands in `playlist` / `liked_track` /
-- `track.rating`. Writing it there would leave two options, both wrong:
-- fabricate local track rows for content that only exists on the server,
-- or silently drop every entry. The first corrupts the library, the
-- second makes synchronization look broken while reporting success.
--
-- So the projection gets its own tables. Everything below is
-- RECONSTRUCTIBLE: dropping the lot and re-fetching `GET
-- /api/v2/sync/snapshot` is always a valid recovery, and is exactly what
-- the apply path does when it meets a known event it cannot apply.
-- `remote_mutation` is the one exception — it holds writes the user made
-- offline that the server has not seen yet, so it is NOT reconstructible
-- and must survive a projection reset.
--
-- ## Identifiers are opaque strings
--
-- RFC-005 §Decision 4: never parse a remote identifier into a UUID.
-- WaveFlow happens to serialize UUIDs, but a third-party Subsonic server
-- emits textual identifiers of another shape, and the desktop must
-- support both. Every `*_remote_id` column below is therefore TEXT with
-- no format constraint.
--
-- The RFC keys remote entities on `(profile_id, remote_id)` because two
-- servers can legitimately emit the same identifier. Here the profile
-- scope is implicit: this migration runs against the per-profile
-- `data.db`, so `remote_id` alone is already profile-scoped, and one
-- profile binds to exactly one server account.

-- =============================================================================
-- 1. Binding — which server this profile talks to, and where it is up to
-- =============================================================================
--
-- Single row, pinned by `CHECK (id = 1)`. The binding is per-profile
-- rather than app-wide (which is where v1 kept the server URL): with a
-- universal client, two profiles may legitimately point at two different
-- servers, and the cursor/device pair is meaningless across accounts.
--
-- `flavour` decides which of the identity columns carry meaning, and the
-- CHECK enforces that rather than trusting the application layer. A
-- third-party Subsonic server has no account UUID, no device notion and
-- no journal cursor; a WaveFlow server has all three.

CREATE TABLE remote_binding (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    server_url        TEXT    NOT NULL,
    flavour           TEXT    NOT NULL CHECK (flavour IN ('waveflow', 'subsonic')),
    -- WaveFlow branch.
    account_id        TEXT,
    device_id         TEXT,
    -- Subsonic branch.
    username          TEXT,
    -- Journal position. 0 means "never bootstrapped"; the snapshot pass
    -- is what first moves it. Only meaningful on the WaveFlow branch.
    cursor            INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0),
    -- Optional library filter for the catalogue projections. An account
    -- with access to several libraries picks one instead of spawning
    -- artificial profiles (RFC-005 §Decision 3).
    active_library_id TEXT,
    -- NULL until the first snapshot has been applied. Distinguishes "no
    -- data yet because we never asked" from "no data because the account
    -- is empty" — without it, an empty account and a failed bootstrap
    -- look identical.
    bootstrapped_at   INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,

    CHECK (
        (flavour = 'waveflow' AND account_id IS NOT NULL AND device_id IS NOT NULL)
     OR (flavour = 'subsonic' AND username IS NOT NULL)
    )
);

-- =============================================================================
-- 2. Projected user data
-- =============================================================================

CREATE TABLE remote_playlist (
    remote_id  TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    comment    TEXT,
    is_public  INTEGER NOT NULL DEFAULT 0 CHECK (is_public IN (0, 1)),
    created_at INTEGER,
    updated_at INTEGER
);

-- Keyed by `(playlist, position)` rather than `(playlist, track)`: the
-- same track may legitimately appear twice in one playlist, and a PK that
-- forbade it would abort the apply pass on a perfectly valid server
-- state.
CREATE TABLE remote_playlist_track (
    playlist_remote_id TEXT    NOT NULL
                       REFERENCES remote_playlist(remote_id) ON DELETE CASCADE,
    position           INTEGER NOT NULL CHECK (position >= 0),
    track_remote_id    TEXT    NOT NULL,
    PRIMARY KEY (playlist_remote_id, position)
);

CREATE INDEX idx_remote_playlist_track_track
    ON remote_playlist_track (track_remote_id);

-- `entity_type` is stored free-form ON PURPOSE, with no CHECK.
--
-- The server favourites `track`, `album` and `artist` today. A CHECK
-- pinning that list would turn a future fourth kind into an aborted
-- transaction — and per RFC-003 (server) an event the client cannot
-- apply forces it to drop its projection and re-fetch a snapshot, whose
-- replay would abort on the same row. That is an infinite recovery loop
-- triggered by a forward-compatible server change. Storing the string
-- and letting the UI ignore what it cannot render degrades instead.
CREATE TABLE remote_favorite (
    entity_type TEXT    NOT NULL,
    entity_id   TEXT    NOT NULL,
    starred_at  INTEGER NOT NULL,
    PRIMARY KEY (entity_type, entity_id)
);

-- `rating` is constrained to 1..5 because a stored row means "this has a
-- rating": the wire protocol uses 0 as a CLEAR instruction, which the
-- apply path turns into a DELETE rather than a stored zero.
--
-- Unlike `entity_type` above, this CHECK is safe: the apply path clamps
-- the incoming value into range before binding it, so the constraint can
-- only fire on a bug in our own code — which is precisely what a CHECK is
-- for. An out-of-range value from the server degrades to a clamp, not to
-- an aborted transaction.
CREATE TABLE remote_rating (
    entity_type TEXT    NOT NULL,
    entity_id   TEXT    NOT NULL,
    rating      INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5),
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (entity_type, entity_id)
);

-- Keyed by `(track, played_at)` — the natural key — rather than by the
-- scrobble's own identifier.
--
-- The reason is a genuine asymmetry in the server contract: a `scrobble`
-- event on `/sync/changes` carries an `entity_id`, but `snapshot.history[]`
-- carries only `{track_id, submission, played_at}` with no identifier at
-- all. A table keyed on the event identifier could therefore not be
-- populated from a snapshot, and the same scrobble would land twice —
-- once anonymously from the snapshot, once identified from a later event.
-- The natural key is present in both shapes, so it is the one that
-- converges.
CREATE TABLE remote_history (
    track_remote_id TEXT    NOT NULL,
    played_at       INTEGER NOT NULL,
    submission      INTEGER NOT NULL CHECK (submission IN (0, 1)),
    PRIMARY KEY (track_remote_id, played_at)
);

CREATE INDEX idx_remote_history_played_at
    ON remote_history (played_at DESC);

-- The account's saved queue: one row, same `CHECK (id = 1)` pin as the
-- binding. Its tracks live in a sibling table so ordering survives
-- without a serialized blob.
CREATE TABLE remote_queue (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    current_remote_id TEXT,
    position_ms       INTEGER NOT NULL DEFAULT 0 CHECK (position_ms >= 0),
    -- Which client last saved the queue. Purely informational — it is
    -- what lets the UI say "paused on your phone" instead of pretending
    -- the position came from here.
    changed_by        TEXT,
    updated_at        INTEGER NOT NULL
);

CREATE TABLE remote_queue_track (
    position        INTEGER PRIMARY KEY CHECK (position >= 0),
    track_remote_id TEXT NOT NULL
);

-- Non-secret share fields only. The journal never carries the bearer
-- token or the public URL (RFC-003 server, §changes table), so a share
-- created on another device arrives here WITHOUT a usable link — and it
-- cannot be derived locally, since the token is keyed on a server-side
-- instance secret.
--
-- `url` is therefore populated only for shares this install created, from
-- the POST response. It is a bearer capability: anyone holding it can
-- read the share, which puts it at the same trust level as the access
-- token already stored in `auth_credential`.
CREATE TABLE remote_share (
    remote_id   TEXT PRIMARY KEY,
    description TEXT,
    expires_at  INTEGER,
    visit_count INTEGER NOT NULL DEFAULT 0 CHECK (visit_count >= 0),
    url         TEXT,
    created_at  INTEGER,
    updated_at  INTEGER
);

CREATE TABLE remote_share_track (
    share_remote_id TEXT    NOT NULL
                    REFERENCES remote_share(remote_id) ON DELETE CASCADE,
    position        INTEGER NOT NULL CHECK (position >= 0),
    track_remote_id TEXT    NOT NULL,
    PRIMARY KEY (share_remote_id, position)
);

-- =============================================================================
-- 3. Outbound queue
-- =============================================================================
--
-- Replayable business mutations, not generic ops: each row is one REST
-- call the drain re-issues verbatim under its `operation_id`, which the
-- server reads from `X-WaveFlow-Operation-Id` to recognize a replay.
--
-- `operation_id` is UNIQUE and IMMUTABLE. The server stores a canonical
-- fingerprint of the action, target and normalized payload; presenting
-- the same identifier with a different fingerprint is rejected as a
-- conflict, not accepted as a replay. So a user who corrects a gesture
-- before the queue drains (renaming an already-queued playlist) needs a
-- NEW identifier — either a second row, or a merge performed before
-- enqueueing that regenerates the id. Updating `payload` in place while
-- keeping `operation_id` is the one thing this table must never do.
--
-- `AUTOINCREMENT` for the reason the v1 queue documented: without it
-- SQLite reuses rowids after deletes, so a drain tracking a
-- last-processed high-water mark would silently skip rows appended after
-- the queue fully drained.
--
-- `failed_at` marks a PERMANENT failure. The server answers 422
-- `validation_error` both for a malformed payload and for an
-- operation-id conflict; neither can succeed on retry, so the row stops
-- being attempted and is surfaced to the user instead of spinning
-- forever. Only 5xx, 429 and transport errors keep their retry.
CREATE TABLE remote_mutation (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    operation_id    TEXT    NOT NULL UNIQUE,
    kind            TEXT    NOT NULL,
    payload         TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    attempt_count   INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_attempt_at INTEGER,
    last_error      TEXT,
    failed_at       INTEGER
);

-- The drain walks pending rows in FIFO order; the partial index keeps
-- permanently-failed rows out of that scan while leaving them readable
-- for the diagnostics surface.
CREATE INDEX idx_remote_mutation_pending
    ON remote_mutation (id) WHERE failed_at IS NULL;
