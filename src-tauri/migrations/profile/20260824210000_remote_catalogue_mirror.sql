-- The server's catalogue, mirrored locally so it can be browsed.
--
-- Until now `remote_track` only ever held what the *user data* referenced:
-- the snapshot carries full song objects for playlists and the queue, and
-- `GET /api/v2/sync/changes` carries bare identifiers. A track the account
-- never touched was therefore invisible, which is why the remote source can
-- only show playlists — there is no "all the server's albums" to show.
--
-- Browsing both sources from one library needs the catalogue itself, and
-- needs it in SQL: merging a local table with a paginated HTTP endpoint
-- cannot be sorted, filtered or virtualised as one list. So the catalogue
-- is walked once and mirrored here, and every list afterwards is a query.
--
-- Still a cache, on the same terms as the rest of the projection: dropping
-- these rows and walking again is always a valid recovery, and nothing here
-- references the local `track` table.

-- Which rows came from the catalogue walk rather than from user data.
--
-- The distinction matters on the way out, not on the way in: purging the
-- mirror must not delete the rows a playlist needs to render its titles,
-- and a track that is both keeps its row when the mirror is dropped. The
-- default of 0 is correct for every row that already exists — they all
-- arrived through the snapshot.
ALTER TABLE remote_track ADD COLUMN in_catalogue INTEGER NOT NULL DEFAULT 0;

-- Albums as the server describes them, rather than as we could group them.
--
-- Deriving albums from `remote_track` by `album_id` would work and would
-- also lose everything the grouping cannot see: whether the album is a
-- compilation, its tagged sort form, and its authoritative track count.
-- That last one is what makes the walk incremental — an album whose
-- `song_count` matches what we already mirrored is skipped without being
-- fetched.
CREATE TABLE remote_album (
    remote_id      TEXT    PRIMARY KEY,
    library_id     TEXT,
    title          TEXT    NOT NULL,
    artist         TEXT,
    artist_id      TEXT,
    artwork_hash   TEXT,
    year           INTEGER,
    is_compilation INTEGER NOT NULL DEFAULT 0 CHECK (is_compilation IN (0, 1)),
    -- Tagged sort form, when the server has one. Sorting on it rather than
    -- on the title is what puts "The Beatles" under B.
    sort_name      TEXT,
    -- The server's count of *available* tracks, and their total duration.
    -- `song_count` doubles as the freshness check described above.
    song_count     INTEGER NOT NULL DEFAULT 0 CHECK (song_count >= 0),
    duration_ms    INTEGER NOT NULL DEFAULT 0 CHECK (duration_ms >= 0),
    -- When the server first saw the album. Drives "recently added".
    created_at     INTEGER,
    -- When we last walked this album's tracks. NULL until it is walked, so
    -- an interrupted mirror resumes where it stopped instead of restarting.
    mirrored_at    INTEGER
);

CREATE INDEX idx_remote_album_artist ON remote_album (artist_id);
CREATE INDEX idx_remote_album_mirrored ON remote_album (mirrored_at);

-- The libraries this account can see. Their names are the only place a
-- multi-library server's structure survives locally, and the catalogue
-- walk needs the identifiers to sweep each one for tracks that belong to
-- no album.
CREATE TABLE remote_library (
    remote_id   TEXT    PRIMARY KEY,
    name        TEXT    NOT NULL,
    -- When this library's sweep last completed. NULL means never.
    mirrored_at INTEGER
);
