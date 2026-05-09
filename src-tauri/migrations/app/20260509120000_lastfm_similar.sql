-- =============================================================================
-- Similar-artists cache (Last.fm + Deezer fallback).
--
-- Cached because:
--   * the upstream payload is identical for every profile;
--   * Last.fm rate-limits anonymous reads to ~5 req/s;
--   * the "similar" carousel on ArtistDetailView would otherwise re-fetch on
--     every navigation.
--
-- Keyed by the source artist's canonical name (lowercased + punctuation
-- stripped, same routine the scanner uses for `artist.canonical_name`) so
-- "The Beatles" / "the beatles!" hit the same row regardless of how the
-- user's tags spell the artist.
--
-- `payload` is a JSON array of `{name, match, picture_url}` rendered by
-- the frontend. We don't normalise it into a relational shape — there's no
-- query that selects across multiple source artists, and the full row
-- ships to the UI in one shot.
-- =============================================================================

CREATE TABLE lastfm_similar (
    name_canonical  TEXT PRIMARY KEY,
    payload         TEXT NOT NULL,
    source          TEXT NOT NULL CHECK (source IN ('lastfm','deezer')),
    fetched_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL
);

CREATE INDEX idx_lastfm_similar_expires ON lastfm_similar(expires_at);
