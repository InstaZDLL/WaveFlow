-- Release Radar: the recent releases MusicBrainz knows of for the artists
-- of the library, and what the radar has learnt about those artists.
--
-- Two tables of their own, next to `artist` rather than in it: what the
-- radar learns (a MusicBrainz id read off a release's credit, when it last
-- asked about an artist) is the radar's working state, and the library's
-- own identifiers are filled by the enrichment, not by this. Both go with
-- the artist they point at (`ON DELETE CASCADE`), so an artist that leaves
-- the library takes its releases with it.

-- One row per artist the radar has asked MusicBrainz about.
CREATE TABLE release_radar_artist (
    artist_id   INTEGER PRIMARY KEY REFERENCES artist(id) ON DELETE CASCADE,
    -- The MusicBrainz artist id, once a release credit has named it
    -- unambiguously. NULL until then: the next query searches by name.
    mbid        TEXT,
    -- When MusicBrainz last answered for this artist (epoch ms). NULL
    -- means "ask on the next scan": a scan stopped halfway resumes with
    -- the artists it did not reach, and a full rescan clears the column.
    checked_at  INTEGER
);

-- One row per release group, keyed by its MusicBrainz id.
CREATE TABLE release_radar_release (
    id                  TEXT PRIMARY KEY,
    -- The library artist the release was found for.
    artist_id           INTEGER NOT NULL REFERENCES artist(id) ON DELETE CASCADE,
    title               TEXT NOT NULL,
    -- The whole credit as MusicBrainz writes it ("A feat. B").
    artist_credit       TEXT NOT NULL,
    -- `YYYY-MM-DD`, `YYYY-MM` or `YYYY`, as MusicBrainz reports it.
    first_release_date  TEXT NOT NULL,
    -- Album / EP / Single / Broadcast / Other, or NULL.
    primary_type        TEXT,
    -- Compilation, Live, Remix… joined on "; ", or NULL.
    secondary_types     TEXT,
    -- The Cover Art Archive front, cached in `metadata_artwork/`.
    cover_hash          TEXT,
    -- When the cover was last asked for (epoch ms); NULL = never.
    cover_checked_at    INTEGER,
    discovered_at       INTEGER NOT NULL
);

CREATE INDEX idx_release_radar_release_artist ON release_radar_release(artist_id);
CREATE INDEX idx_release_radar_release_date ON release_radar_release(first_release_date DESC);
