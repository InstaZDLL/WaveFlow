-- Per-artist manual overrides for bio + similar artists (issue #323).
--
-- Offline-first users want the same manual control over an artist's
-- biography and similar-artist list that they already have over the
-- artist picture (local `artist.jpg` sidecar). Both overrides are
-- per-profile and live in the profile DB, so an enrichment pass
-- (Deezer / Last.fm / TheAudioDB) — which writes the shared
-- `app.metadata_artist` cache — never touches them. The read paths
-- short-circuit to these when present.

-- Free-text biography. NULL = no override (use the fetched bio).
ALTER TABLE artist ADD COLUMN custom_bio TEXT;

-- User-curated similar artists, scoped to the library by design: each
-- row points at another local `artist` row. `position` keeps the
-- display order the user picked. Absence of any row for a given
-- `artist_id` = no override (fall back to the online similar list).
CREATE TABLE artist_similar_custom (
    artist_id          INTEGER NOT NULL REFERENCES artist(id) ON DELETE CASCADE,
    similar_artist_id  INTEGER NOT NULL REFERENCES artist(id) ON DELETE CASCADE,
    position           INTEGER NOT NULL,
    PRIMARY KEY (artist_id, similar_artist_id)
);

-- Fetch + order a single artist's curated list without scanning.
CREATE INDEX idx_artist_similar_custom_lookup
    ON artist_similar_custom(artist_id, position);
