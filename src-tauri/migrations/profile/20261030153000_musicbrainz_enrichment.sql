-- The library enrichment engine: MusicBrainz identifiers, and what was
-- filled from them.
--
-- Side tables rather than columns on `album` / `track` / `artist`, for
-- two reasons. `track` and `album` carry the FTS triggers and are
-- rewritten by every scan; an identifier written next to them would
-- have to survive the scanner's upserts, which know nothing about it.
-- And each row here is a *decision* — applied, waiting for review,
-- rejected, no match — with the evidence the review screen shows, not
-- just an identifier. `ON DELETE CASCADE` retires the decision with the
-- row it was about: an album the scanner prunes takes its match along.
--
-- Nothing here is written into an audio file. The engine fills gaps in
-- the database only; the file's own tags win on the next scan of a
-- rewritten file, and the engine re-fills what that scan emptied.

-- One decision per local album.
CREATE TABLE musicbrainz_album (
    album_id            INTEGER PRIMARY KEY REFERENCES album(id) ON DELETE CASCADE,
    -- `applied`: confident, filled. `review`: doubtful, waiting for the
    -- user. `rejected`: the user said no, never proposed again on its
    -- own. `no_match`: nothing plausible, asked again after a while.
    status              TEXT NOT NULL
                        CHECK (status IN ('applied', 'review', 'rejected', 'no_match')),
    score               REAL,
    release_mbid        TEXT,
    release_group_mbid  TEXT,
    -- The candidate as the review screen shows it.
    release_title       TEXT,
    release_artist      TEXT,
    release_country     TEXT,
    release_track_count INTEGER,
    -- What the engine fills from it: the release group's first release
    -- date, its year, and its genres (a JSON array of names).
    original_date       TEXT,
    original_year       INTEGER,
    genres              TEXT,
    -- Whether Cover Art Archive holds a front cover for the release.
    has_front_cover     INTEGER NOT NULL DEFAULT 0,
    -- What this decision actually wrote, so an undo removes that and
    -- nothing else.
    filled_year         INTEGER NOT NULL DEFAULT 0,
    filled_release_date INTEGER NOT NULL DEFAULT 0,
    filled_cover        INTEGER NOT NULL DEFAULT 0,
    matched_at          INTEGER NOT NULL
);

CREATE INDEX idx_musicbrainz_album_status ON musicbrainz_album(status);

-- The recording a local track was paired with, inside its album's match.
CREATE TABLE musicbrainz_track (
    track_id        INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    album_id        INTEGER NOT NULL REFERENCES musicbrainz_album(album_id) ON DELETE CASCADE,
    recording_mbid  TEXT NOT NULL,
    score           REAL NOT NULL,
    filled_year     INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_musicbrainz_track_album ON musicbrainz_track(album_id);

-- The genres the engine gave a track that had none. Kept apart from
-- `track_genre`, which the scanner empties and refills from the file,
-- so the engine can tell its own rows from the file's and re-fill a
-- track a scan emptied.
CREATE TABLE musicbrainz_filled_genre (
    track_id    INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    genre_id    INTEGER NOT NULL REFERENCES genre(id) ON DELETE CASCADE,
    PRIMARY KEY (track_id, genre_id)
);

-- Performers, composers, producers: names, not `artist` rows. A credit
-- is not an artist of the library — making one a row would put every
-- session drummer into the artist list.
CREATE TABLE track_credit (
    track_id    INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    -- MusicBrainz's relationship type: `composer`, `lyricist`,
    -- `producer`, `instrument`, `vocal`, …
    role        TEXT NOT NULL,
    name        TEXT NOT NULL,
    -- The instrument or the vocal part, where the role has one.
    detail      TEXT NOT NULL DEFAULT '',
    artist_mbid TEXT,
    position    INTEGER NOT NULL,
    PRIMARY KEY (track_id, role, name, detail)
);

-- One decision per library artist. Never applied on its own: an artist
-- name is too ambiguous for that, so every match waits for the user.
CREATE TABLE musicbrainz_artist (
    artist_id       INTEGER PRIMARY KEY REFERENCES artist(id) ON DELETE CASCADE,
    status          TEXT NOT NULL
                    CHECK (status IN ('applied', 'review', 'rejected', 'no_match')),
    score           REAL,
    artist_mbid     TEXT,
    name            TEXT,
    disambiguation  TEXT,
    kind            TEXT,
    country         TEXT,
    area            TEXT,
    begin_date      TEXT,
    end_date        TEXT,
    genres          TEXT,
    -- How many matched albums credit this MusicBrainz artist under this
    -- name: the evidence behind the proposal, shown on review.
    album_evidence  INTEGER NOT NULL DEFAULT 0,
    matched_at      INTEGER NOT NULL
);

CREATE INDEX idx_musicbrainz_artist_status ON musicbrainz_artist(status);
