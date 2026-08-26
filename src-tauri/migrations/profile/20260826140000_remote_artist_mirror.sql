-- The server's artists, mirrored like its albums.
--
-- The album walk already lands an artist's *name* on every track and album
-- row, so a unified artist listing could be produced by grouping on
-- `artist_id` alone. What grouping cannot produce is the artist's **picture**:
-- it lives on the server's artist row and is fetched today, per artist, by
-- the remote artist view. A grid built from a GROUP BY would therefore show
-- letters where the local half shows photographs — which is precisely the
-- defect issue #350 was about, arriving by a different route.
--
-- So artists are walked and mirrored, on the same terms as albums: derived
-- from the server, droppable, reconstructible by walking again.
--
-- Counts are deliberately *not* stored. An artist's track and album totals are
-- already implied by the rows this mirror holds, and a stored count is a
-- second truth that goes stale the moment an album is walked.
CREATE TABLE remote_artist (
    remote_id    TEXT    PRIMARY KEY,
    library_id   TEXT,
    name         TEXT    NOT NULL,
    artwork_hash TEXT,
    -- The server's tagged sort form, kept verbatim for display decisions.
    sort_name    TEXT,
    -- The comparison key, normalised by `name_match::normalize_name` so it
    -- sorts against the local half's `artist.canonical_name`. SQLite cannot
    -- fold a diacritic, so without this "Björk" and "bjork" land in two
    -- different places in one list — see the album sort keys migration.
    sort_key     TEXT,
    mirrored_at  INTEGER
);

CREATE INDEX idx_remote_artist_sort ON remote_artist (sort_key);
