-- Web Radio now-playing lyrics cache.
--
-- A radio session has no library `track` row (negative sentinel id, live
-- stream, no file on disk), so the file-hash-keyed `lyrics` table can't
-- hold its lyrics. This table caches lyrics fetched for a radio song by
-- the (artist, title) pair parsed from the ICY `StreamTitle`.
--
-- Lives in app.db (shared across profiles, like `lyrics`) — the same
-- song on the same station yields the same lyrics regardless of which
-- profile is listening.
--
-- `artist_title_key` is blake3(lower(artist) || \x1f || lower(title)) so
-- casing / separator noise in ICY titles still collapses to one row.
-- An empty `content` is a cached MISS: nothing matched, don't re-hit the
-- network the next time this song recurs on a station's rotation.
CREATE TABLE radio_lyrics (
    artist_title_key TEXT PRIMARY KEY,
    artist           TEXT NOT NULL,
    title            TEXT NOT NULL,
    content          TEXT NOT NULL,
    format           TEXT NOT NULL CHECK (format IN ('plain', 'lrc', 'enhanced_lrc', 'ttml')),
    source           TEXT NOT NULL CHECK (source IN ('api', 'manual')),
    provider         TEXT,
    fetched_at       INTEGER NOT NULL
);
