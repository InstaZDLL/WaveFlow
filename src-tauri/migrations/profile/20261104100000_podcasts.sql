-- Podcasts (RFC-009): the channels a profile follows and their episodes.
--
-- An episode is not a track. It has no `track` row and touches no table of
-- the library, so it stays out of the statistics, Wrapped, scrobbling, smart
-- playlists, duplicates and enrichment without any of them having to know.
--
-- Every address column here is nullable, because an exported archive blanks
-- them (`profile_io`): a private feed's address holds a token, and an archive
-- is made to travel. `feed_key` and `item_key` are hashes that survive that,
-- so subscribing again to the same address finds the channel and the episodes
-- with their positions intact. A key is also why no guid is stored: a guid is
-- often the enclosure's address, and would be one more copy of the token.
--
-- `last_error` holds one word from the list in `FetchFailure::code`, and no
-- CHECK enforces it: the list will grow, and a CHECK could only follow by
-- rebuilding the table.
CREATE TABLE podcast_channel (
    id                 INTEGER PRIMARY KEY,
    feed_key           TEXT NOT NULL UNIQUE,
    feed_url           TEXT,
    title              TEXT NOT NULL,
    description        TEXT,
    author             TEXT,
    language           TEXT,
    site_url           TEXT,
    image_url          TEXT,
    -- The channel's picture in the shared artwork cache, so that opening the
    -- page contacts nobody.
    artwork_hash       TEXT,
    -- What the host answered last, sent back so an unchanged feed costs a 304.
    etag               TEXT,
    last_modified      TEXT,
    last_refreshed_at  INTEGER,
    last_error         TEXT,
    -- The speed its episodes start at; NULL is the normal one. Here and not in
    -- `audio.playback_speed`, so a speed chosen for a voice never reaches music.
    playback_speed     REAL,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL
);

CREATE TABLE podcast_episode (
    id                     INTEGER PRIMARY KEY,
    channel_id             INTEGER NOT NULL REFERENCES podcast_channel(id) ON DELETE CASCADE,
    item_key               TEXT NOT NULL,
    enclosure_url          TEXT,
    enclosure_type         TEXT,
    enclosure_length       INTEGER,
    title                  TEXT NOT NULL,
    description            TEXT,
    published_at           INTEGER,
    duration_ms            INTEGER,
    season                 INTEGER,
    episode_number         INTEGER,
    first_seen_at          INTEGER NOT NULL,
    updated_at             INTEGER NOT NULL,
    -- The user removed it. The row stays so a refresh does not bring it back.
    deleted_at             INTEGER,

    -- Resume: where it was left, and when it counted as played.
    position_ms            INTEGER NOT NULL DEFAULT 0,
    played_at              INTEGER,
    last_opened_at         INTEGER,

    -- The downloaded file, by name inside the managed folder. Never a path out
    -- of a feed, and never in a library folder.
    download_file          TEXT,
    download_size          INTEGER,
    downloaded_at          INTEGER,

    -- `podcast:chapters`, `psc:chapters` as JSON, and `podcast:transcript` as
    -- JSON: what the feed offers. Addresses inside the last, so it is blanked
    -- in an archive with the others.
    chapters_url           TEXT,
    inline_chapters        TEXT,
    transcripts            TEXT,

    -- What was fetched when the episode or the panel was opened. Last in the
    -- row and never read by a list: a transcript may be megabytes.
    chapters_checked_at    INTEGER,
    chapters_cache         TEXT,
    transcript_checked_at  INTEGER,
    transcript_kind        TEXT,
    transcript_cache       TEXT,

    UNIQUE (channel_id, item_key)
);

CREATE INDEX idx_podcast_episode_channel_published
    ON podcast_episode (channel_id, published_at DESC);
