# Integrations

External services WaveFlow talks to. All clients use [`reqwest 0.12`](https://crates.io/crates/reqwest) with `rustls-tls` so there's no system OpenSSL dependency.

## Deezer (metadata)

[`deezer.rs`](../../src-tauri/src/deezer.rs) — public Deezer API, no auth. Used for:

- Artist pictures (`enrich_artist_deezer`)
- Album covers (`enrich_album_deezer`, `search_albums_deezer`, `set_album_artwork_from_deezer`)
- Label / fan-count metadata

Results are cached in the `deezer_artist` / `deezer_album` tables of the **shared** `app.db` (one cache across every profile) with a 30-day `expires_at` TTL. Cache-first: zero network round-trips when the row is fresh. Failures are non-fatal — the UI degrades to local-only artwork and an empty enrichment payload.

Downloaded images go through [`metadata_artwork::download_and_cache`](../../src-tauri/src/metadata_artwork.rs): Blake3-hashed bytes → `<root>/metadata_artwork/<hash>.jpg`. The hash is persisted in `deezer_artist.picture_hash` / `deezer_album.cover_hash` so a cache hit on the metadata table avoids re-downloading. Thumbnails (1×, 2×) are generated asynchronously by [`thumbnails.rs`](../../src-tauri/src/thumbnails.rs).

The frontend helper `lib/tauri/artwork.ts::resolveRemoteImage` prefers the local file via `convertFileSrc` so artist imagery renders offline. The `metadata_artwork/**` scope must stay listed in `tauri.conf.json` `assetProtocol`.

## Last.fm

[`lastfm.rs`](../../src-tauri/src/lastfm.rs) — split into two flows:

### Read-only (artist bios)

`artist.getInfo` for biographies, called from `enrich_artist_deezer` after the Deezer pass. Cached in the same `deezer_artist` row (the table name is historical — it holds Last.fm bios too) with the same 30-day TTL. **Optional**: requires a user-supplied API key in `app_setting['lastfm_api_key']`. Without it, bios are skipped silently and the UI shows local data.

### Authenticated (scrobbling)

[`scrobbler.rs`](../../src-tauri/src/scrobbler.rs) is the worker thread that drives Last.fm scrobbles:

- **Login** — signed `auth.getMobileSession` (md-5 of params + secret). Session key persisted in `app_setting['lastfm_session_key']`.
- **Now Playing** — `track.updateNowPlaying` fires on every `player:track-changed` event after the 240 s threshold. Best-effort; failures are logged but never block playback.
- **Scrobble queue** — `track.scrobble` is queued in the per-profile `scrobble_queue` table and drained with exponential backoff (10 s → 5 min). Survives app restarts.
- **Re-auth** — on `9` (`Invalid session key`) or `4` (`Authentication failed`), the session is wiped and a `lastfm:reauth` event is emitted. The frontend surfaces a banner (`LastfmReauthBanner`) with a one-click "Re-authenticate" button.

## LRCLIB (synchronized lyrics)

[`lrclib.rs`](../../src-tauri/src/lrclib.rs) — public lookup by `artist_name + track_name + album_name + duration` against [LRCLIB](https://lrclib.net). Three-tier resolution in [`commands/lyrics.rs`](../../src-tauri/src/commands/lyrics.rs):

1. **Embedded** — `LYRICS` / `USLT` tag in the file (lofty), incl. synced `LRC` blocks.
2. **Local file** — `<track_basename>.lrc` next to the audio file.
3. **LRCLIB** — synced first, falls back to plain. Cached in the `lyrics` table of `app.db` (shared) with no TTL — lyrics don't change.

A failed LRCLIB lookup is cached as an empty row to suppress retries on the same track for a session. `clear_lyrics` flushes that row if the user wants to retry manually after fixing tags.

The lyrics panel renders synced lines with auto-scroll and a 200 ms transition; un-synced lyrics fall back to a static block.
