-- Cached metadata for the server's tracks. RFC-005.
--
-- The projection references tracks by remote identifier, and the two
-- feeds that populate it are asymmetric:
--
--   * `GET /api/v2/sync/snapshot` returns playlists and the queue with
--     FULL song objects — title, artist, album, duration, artwork.
--   * `GET /api/v2/sync/changes` carries a playlist upsert as an ordered
--     list of `track_ids` and nothing else.
--
-- So a playlist edited after the bootstrap would arrive as identifiers
-- with no way to render them. This table keeps what the snapshot handed
-- us for free, and the apply path fetches `GET /api/v2/tracks/{id}` only
-- for identifiers it has never seen.
--
-- Purely a cache: it is derived from the server and can be dropped and
-- refilled at any time. It deliberately does NOT reference `track` —
-- these rows describe content that exists only on the server, and
-- matching them to local files is out of scope (RFC-005 §Decision 1).
--
-- Nullable almost everywhere because a third-party server fills in far
-- less than ours does, and a NOT NULL here would turn a sparse catalogue
-- into a failed sync. `title` and `duration_ms` are the two the UI
-- cannot render without.

CREATE TABLE remote_track (
    remote_id    TEXT    PRIMARY KEY,
    title        TEXT    NOT NULL,
    artist       TEXT,
    album        TEXT,
    album_id     TEXT,
    duration_ms  INTEGER NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    track_no     INTEGER,
    disc_no      INTEGER,
    year         INTEGER,
    genre        TEXT,
    suffix       TEXT,
    bitrate      INTEGER,
    size         INTEGER,
    artwork_hash TEXT,
    library_id   TEXT,
    -- When we last refreshed this row from the server. Lets a future
    -- pass expire stale entries without having to re-fetch everything.
    cached_at    INTEGER NOT NULL
);

CREATE INDEX idx_remote_track_album ON remote_track (album_id);
