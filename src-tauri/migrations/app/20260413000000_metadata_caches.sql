-- =============================================================================
-- Shared metadata caches.
--
-- These tables hold information that is identical for every user — Deezer
-- artist/album metadata (keyed by Deezer's stable IDs) and song lyrics
-- (keyed by the audio file content hash). Storing them per-profile would
-- duplicate identical rows across multiple `data.db` files, so they live
-- in `app.db` instead.
--
-- The per-profile pool ATTACHes this database as `app` on every connection,
-- so queries can JOIN across boundaries (e.g.
-- `LEFT JOIN app.deezer_artist da ON da.deezer_id = ar.deezer_id`).
-- =============================================================================

-- Cached Deezer artist responses + Last.fm bio enrichment ---------------------
CREATE TABLE deezer_artist (
    deezer_id       INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    picture_url     TEXT,
    picture_hash    TEXT,
    bio_short       TEXT,
    bio_full        TEXT,
    fans_count      INTEGER,
    albums_count    INTEGER,
    tracklist_url   TEXT,
    fetched_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL
);

CREATE INDEX idx_deezer_artist_expires ON deezer_artist(expires_at);

-- Cached Deezer album responses ------------------------------------------------
CREATE TABLE deezer_album (
    deezer_id            INTEGER PRIMARY KEY,
    title                TEXT NOT NULL,
    artist_deezer_id     INTEGER,
    release_date         TEXT,
    cover_url            TEXT,
    cover_hash           TEXT,
    tracks_count         INTEGER,
    label                TEXT,
    fetched_at           INTEGER NOT NULL,
    expires_at           INTEGER NOT NULL
);

CREATE INDEX idx_deezer_album_expires ON deezer_album(expires_at);

-- Lyrics cache, keyed by audio file content hash so the same recording shared
-- between profiles is fetched once. blake3 is computed during scan and stored
-- in `track.file_hash` per profile.
CREATE TABLE lyrics (
    file_hash       TEXT PRIMARY KEY,
    content         TEXT NOT NULL,
    format          TEXT NOT NULL CHECK (format IN ('plain','lrc','enhanced_lrc')),
    source          TEXT NOT NULL CHECK (source IN ('embedded','lrc_file','api','manual')),
    language        TEXT,
    fetched_at      INTEGER NOT NULL
);
