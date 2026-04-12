-- =============================================================================
-- profile_data.db — initial schema
-- One database per profile. Contains libraries, tracks, playlists, queue,
-- history, analytics, scrobble queue and external metadata cache.
-- =============================================================================

-- =============================================================================
-- 1. Profile-scoped settings
-- =============================================================================

CREATE TABLE profile_setting (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL,
    value_type      TEXT NOT NULL CHECK (value_type IN ('string','int','bool','json')),
    updated_at      INTEGER NOT NULL
);

INSERT INTO profile_setting (key, value, value_type, updated_at) VALUES
    ('player.volume',        '80',    'int',    strftime('%s','now') * 1000),
    ('player.shuffle',       'false', 'bool',   strftime('%s','now') * 1000),
    ('player.repeat_mode',   'off',   'string', strftime('%s','now') * 1000),
    ('player.last_track_id', '0',     'int',    strftime('%s','now') * 1000),
    ('player.last_position_ms','0',   'int',    strftime('%s','now') * 1000),
    ('queue.current_index',  '0',     'int',    strftime('%s','now') * 1000);

-- =============================================================================
-- 2. Artwork cache (files live at <profile_dir>/artwork/<hash>.<format>)
-- =============================================================================

CREATE TABLE artwork (
    id              INTEGER PRIMARY KEY,
    hash            TEXT NOT NULL UNIQUE,
    format          TEXT NOT NULL,
    width           INTEGER,
    height          INTEGER,
    source          TEXT NOT NULL CHECK (source IN ('embedded','folder','deezer','manual')),
    created_at      INTEGER NOT NULL
);

-- =============================================================================
-- 3. External provider caches (Deezer first, others follow the same pattern)
-- =============================================================================

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

CREATE TABLE deezer_album (
    deezer_id            INTEGER PRIMARY KEY,
    title                TEXT NOT NULL,
    artist_deezer_id     INTEGER REFERENCES deezer_artist(deezer_id) ON DELETE SET NULL,
    release_date         TEXT,
    cover_url            TEXT,
    cover_hash           TEXT,
    tracks_count         INTEGER,
    label                TEXT,
    fetched_at           INTEGER NOT NULL,
    expires_at           INTEGER NOT NULL
);

CREATE INDEX idx_deezer_album_expires ON deezer_album(expires_at);

-- =============================================================================
-- 4. Library, artists, albums, genres
-- =============================================================================

CREATE TABLE library (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT,
    color_id        TEXT NOT NULL DEFAULT 'emerald',
    icon_id         TEXT NOT NULL DEFAULT 'library',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

CREATE TABLE library_folder (
    id              INTEGER PRIMARY KEY,
    library_id      INTEGER NOT NULL REFERENCES library(id) ON DELETE CASCADE,
    path            TEXT NOT NULL,
    last_scanned_at INTEGER,
    is_watched      INTEGER NOT NULL DEFAULT 0,
    UNIQUE (library_id, path)
);

CREATE INDEX idx_library_folder_library ON library_folder(library_id);
CREATE INDEX idx_library_folder_watched ON library_folder(is_watched) WHERE is_watched = 1;

CREATE TABLE artist (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    canonical_name  TEXT NOT NULL,
    artwork_id      INTEGER REFERENCES artwork(id) ON DELETE SET NULL,
    deezer_id       INTEGER REFERENCES deezer_artist(deezer_id) ON DELETE SET NULL,
    UNIQUE (canonical_name)
);

CREATE INDEX idx_artist_deezer ON artist(deezer_id);

CREATE TABLE album (
    id              INTEGER PRIMARY KEY,
    title           TEXT NOT NULL,
    canonical_title TEXT NOT NULL,
    artist_id       INTEGER REFERENCES artist(id) ON DELETE SET NULL,
    year            INTEGER,
    release_date    TEXT,
    total_tracks    INTEGER,
    total_discs     INTEGER,
    artwork_id      INTEGER REFERENCES artwork(id) ON DELETE SET NULL,
    deezer_id       INTEGER REFERENCES deezer_album(deezer_id) ON DELETE SET NULL,
    UNIQUE (canonical_title, artist_id)
);

CREATE INDEX idx_album_artist ON album(artist_id);

CREATE TABLE genre (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    canonical_name  TEXT NOT NULL UNIQUE
);

-- =============================================================================
-- 5. Tracks + relations
-- =============================================================================

CREATE TABLE track (
    id              INTEGER PRIMARY KEY,
    library_id      INTEGER NOT NULL REFERENCES library(id) ON DELETE CASCADE,
    folder_id       INTEGER REFERENCES library_folder(id) ON DELETE SET NULL,
    file_path       TEXT NOT NULL,
    file_hash       TEXT NOT NULL,
    file_size       INTEGER NOT NULL,
    file_modified   INTEGER NOT NULL,

    title           TEXT NOT NULL,
    album_id        INTEGER REFERENCES album(id) ON DELETE SET NULL,
    primary_artist  INTEGER REFERENCES artist(id) ON DELETE SET NULL,
    track_number    INTEGER,
    disc_number     INTEGER,
    year            INTEGER,

    duration_ms     INTEGER NOT NULL,
    bitrate         INTEGER,
    sample_rate     INTEGER,
    channels        INTEGER,
    codec           TEXT,
    container       TEXT,

    added_at        INTEGER NOT NULL,
    scan_id         INTEGER,
    is_available    INTEGER NOT NULL DEFAULT 1,

    UNIQUE (library_id, file_path)
);

CREATE INDEX idx_track_library       ON track(library_id, is_available);
CREATE INDEX idx_track_folder        ON track(folder_id);
CREATE INDEX idx_track_album         ON track(album_id);
CREATE INDEX idx_track_primary_artist ON track(primary_artist);
CREATE INDEX idx_track_hash          ON track(file_hash);
CREATE INDEX idx_track_added         ON track(added_at DESC);

CREATE TABLE track_artist (
    track_id        INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    artist_id       INTEGER NOT NULL REFERENCES artist(id) ON DELETE CASCADE,
    role            TEXT NOT NULL DEFAULT 'main'
                    CHECK (role IN ('main','feature','remixer','producer','composer')),
    position        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (track_id, artist_id, role)
);

CREATE INDEX idx_track_artist_artist ON track_artist(artist_id);

CREATE TABLE track_genre (
    track_id        INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    genre_id        INTEGER NOT NULL REFERENCES genre(id) ON DELETE CASCADE,
    PRIMARY KEY (track_id, genre_id)
);

CREATE INDEX idx_track_genre_genre ON track_genre(genre_id);

-- =============================================================================
-- 6. Lyrics and audio analysis
-- =============================================================================

CREATE TABLE lyrics (
    track_id        INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    content         TEXT NOT NULL,
    format          TEXT NOT NULL CHECK (format IN ('plain','lrc','enhanced_lrc')),
    source          TEXT NOT NULL CHECK (source IN ('embedded','lrc_file','api','manual')),
    language        TEXT,
    fetched_at      INTEGER NOT NULL
);

CREATE TABLE track_analysis (
    track_id        INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    bpm             REAL,
    musical_key     TEXT,
    loudness_lufs   REAL,
    replay_gain_db  REAL,
    peak            REAL,
    analyzed_at     INTEGER NOT NULL
);

-- =============================================================================
-- 7. Playlists and likes
-- =============================================================================

CREATE TABLE playlist (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT,
    color_id        TEXT NOT NULL DEFAULT 'violet',
    icon_id         TEXT NOT NULL DEFAULT 'music',
    is_smart        INTEGER NOT NULL DEFAULT 0,
    smart_rules     TEXT,
    position        INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

CREATE INDEX idx_playlist_position ON playlist(position);

CREATE TABLE playlist_track (
    playlist_id     INTEGER NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
    track_id        INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,
    added_at        INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, track_id)
);

CREATE INDEX idx_playlist_track_position ON playlist_track(playlist_id, position);

CREATE TABLE liked_track (
    track_id        INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    liked_at        INTEGER NOT NULL
);

CREATE INDEX idx_liked_time ON liked_track(liked_at DESC);

-- =============================================================================
-- 8. Persistent queue
-- =============================================================================

CREATE TABLE queue_item (
    id              INTEGER PRIMARY KEY,
    track_id        INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL UNIQUE,
    source_type     TEXT NOT NULL
                    CHECK (source_type IN ('album','playlist','artist','library','liked','manual','radio')),
    source_id       INTEGER,
    added_at        INTEGER NOT NULL
);

CREATE INDEX idx_queue_position ON queue_item(position);

-- =============================================================================
-- 9. Play history / analytics
-- =============================================================================

CREATE TABLE play_event (
    id              INTEGER PRIMARY KEY,
    track_id        INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    played_at       INTEGER NOT NULL,
    listened_ms     INTEGER NOT NULL,
    completed       INTEGER NOT NULL DEFAULT 0,
    skipped         INTEGER NOT NULL DEFAULT 0,
    source_type     TEXT,
    source_id       INTEGER
);

CREATE INDEX idx_play_event_time  ON play_event(played_at DESC);
CREATE INDEX idx_play_event_track ON play_event(track_id, played_at DESC);

-- =============================================================================
-- 10. Scrobbling (Last.fm / ListenBrainz) — prepared but unused at MVP
-- =============================================================================

CREATE TABLE auth_credential (
    provider                TEXT PRIMARY KEY
                            CHECK (provider IN ('lastfm','listenbrainz','deezer')),
    username                TEXT,
    token_encrypted         BLOB NOT NULL,
    refresh_token_encrypted BLOB,
    expires_at              INTEGER,
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL
);

CREATE TABLE scrobble_queue (
    id              INTEGER PRIMARY KEY,
    provider        TEXT NOT NULL CHECK (provider IN ('lastfm','listenbrainz')),
    track_id        INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    played_at       INTEGER NOT NULL,
    listened_ms     INTEGER NOT NULL,
    retry_count     INTEGER NOT NULL DEFAULT 0,
    next_retry_at   INTEGER,
    last_error      TEXT,
    created_at      INTEGER NOT NULL
);

CREATE INDEX idx_scrobble_queue_retry ON scrobble_queue(next_retry_at) WHERE retry_count < 10;

-- =============================================================================
-- 11. Full-text search (FTS5) on tracks
-- Captures title, album title and primary artist name. Feature artists can
-- be searched through the artist table via separate queries if needed.
-- =============================================================================

CREATE VIRTUAL TABLE track_fts USING fts5(
    title,
    album_title,
    artist_name,
    content='',
    tokenize='unicode61 remove_diacritics 2'
);

-- Keep track_fts in sync with track -----------------------------------------
CREATE TRIGGER track_fts_insert AFTER INSERT ON track BEGIN
    INSERT INTO track_fts (rowid, title, album_title, artist_name) VALUES (
        new.id,
        new.title,
        COALESCE((SELECT title FROM album  WHERE id = new.album_id),       ''),
        COALESCE((SELECT name  FROM artist WHERE id = new.primary_artist), '')
    );
END;

-- `track_fts` is declared contentless (content=''), so DELETE / UPDATE
-- on it must use the special FTS5 `'delete'` / `'delete-all'` commands
-- with the OLD values of the indexed columns. See:
-- https://www.sqlite.org/fts5.html#the_delete_command
CREATE TRIGGER track_fts_delete AFTER DELETE ON track BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, album_title, artist_name)
    VALUES(
        'delete',
        old.id,
        old.title,
        COALESCE((SELECT title FROM album  WHERE id = old.album_id),       ''),
        COALESCE((SELECT name  FROM artist WHERE id = old.primary_artist), '')
    );
END;

CREATE TRIGGER track_fts_update AFTER UPDATE OF title, album_id, primary_artist ON track BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, album_title, artist_name)
    VALUES(
        'delete',
        old.id,
        old.title,
        COALESCE((SELECT title FROM album  WHERE id = old.album_id),       ''),
        COALESCE((SELECT name  FROM artist WHERE id = old.primary_artist), '')
    );
    INSERT INTO track_fts (rowid, title, album_title, artist_name) VALUES (
        new.id,
        new.title,
        COALESCE((SELECT title FROM album  WHERE id = new.album_id),       ''),
        COALESCE((SELECT name  FROM artist WHERE id = new.primary_artist), '')
    );
END;

-- Keep FTS in sync when the denormalized fields change upstream.
-- Contentless FTS5 tables don't support UPDATE either — re-emit the
-- affected rows via delete + insert. Each trigger loops through the
-- matched tracks and re-syncs their FTS row.
CREATE TRIGGER album_title_fts_update AFTER UPDATE OF title ON album BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, album_title, artist_name)
    SELECT 'delete', t.id,
           t.title,
           old.title,
           COALESCE((SELECT name FROM artist WHERE id = t.primary_artist), '')
      FROM track t WHERE t.album_id = new.id;
    INSERT INTO track_fts (rowid, title, album_title, artist_name)
    SELECT t.id,
           t.title,
           new.title,
           COALESCE((SELECT name FROM artist WHERE id = t.primary_artist), '')
      FROM track t WHERE t.album_id = new.id;
END;

CREATE TRIGGER artist_name_fts_update AFTER UPDATE OF name ON artist BEGIN
    INSERT INTO track_fts(track_fts, rowid, title, album_title, artist_name)
    SELECT 'delete', t.id,
           t.title,
           COALESCE((SELECT title FROM album WHERE id = t.album_id), ''),
           old.name
      FROM track t WHERE t.primary_artist = new.id;
    INSERT INTO track_fts (rowid, title, album_title, artist_name)
    SELECT t.id,
           t.title,
           COALESCE((SELECT title FROM album WHERE id = t.album_id), ''),
           new.name
      FROM track t WHERE t.primary_artist = new.id;
END;
