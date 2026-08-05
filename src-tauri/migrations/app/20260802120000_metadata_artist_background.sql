-- Wide artist backdrop (TheAudioDB fanart) for the Spotify-style artist
-- hero — issue #482. Mirrors the existing `picture_url` / `picture_hash`
-- pair: the URL is kept as a remote fallback, the blake3 hash addresses
-- the downloaded file in the shared `metadata_artwork/` cache.
--
-- `background_fetched_at` is the "we already looked" marker and is what
-- makes the lookup cheap: an artist with no fanart on TheAudioDB stores
-- NULL in both other columns, which is indistinguishable from "never
-- queried" without it — and TheAudioDB's free key is rate-limited, so
-- re-querying every artist page visit is not an option. Rows written
-- before this migration keep NULL here and are treated as a background
-- cache miss on their next refresh, which backfills them once.
ALTER TABLE metadata_artist ADD COLUMN background_url TEXT;
ALTER TABLE metadata_artist ADD COLUMN background_hash TEXT;
ALTER TABLE metadata_artist ADD COLUMN background_fetched_at INTEGER;
