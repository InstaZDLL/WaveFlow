# Integrations

External services WaveFlow talks to. All clients use [`reqwest 0.12`](https://crates.io/crates/reqwest) with `rustls-tls` so there's no system OpenSSL dependency.

## Deezer (metadata)

[`deezer.rs`](../../src-tauri/src/deezer.rs) — public Deezer API, no auth. Used for:

- Artist pictures (`enrich_artist_deezer`, `batch_fetch_missing_artist_pictures`)
- Album covers (`enrich_album_deezer`, `search_albums_deezer`, `set_album_artwork_from_deezer`, `batch_fetch_missing_album_covers`)
- Label / fan-count metadata

Results are cached in the `deezer_artist` / `deezer_album` tables of the **shared** `app.db` (one cache across every profile) with a 30-day `expires_at` TTL. Cache-first: zero network round-trips when the row is fresh. Failures are non-fatal — the UI degrades to local-only artwork and an empty enrichment payload.

**Auto-enrichment on play.** [`PlayerProvider`](../../src/contexts/PlayerContext.tsx) fires `enrich_artist_deezer(currentTrack.artist_id)` (fire-and-forget) on every track-change. Cache hits are ~10 ms so the duplicate call done by `NowPlayingPanel` when it renders is harmless; the point is to populate the cache for views the user *isn't* looking at right now (e.g. the artist grid in `LibraryView`) so a tile gets its picture as soon as the user plays one of that artist's tracks, regardless of whether the Now Playing panel is open.

**Batch fill-in.** `batch_fetch_missing_artist_pictures` walks every artist with no cached row (or an expired one), runs the standard enrichment per artist, and emits `artist-fetch-progress` so a Settings progress bar can drive the UI. Throttled at 200 ms (~5 req/s) to stay well below Deezer's anonymous rate limit. Same idempotent semantics as `batch_fetch_missing_album_covers`: re-running just resumes on whatever's still missing.

Downloaded images go through [`metadata_artwork::download_and_cache`](../../src-tauri/src/metadata_artwork.rs): Blake3-hashed bytes → `<root>/metadata_artwork/<hash>.jpg`. The hash is persisted in `deezer_artist.picture_hash` / `deezer_album.cover_hash` so a cache hit on the metadata table avoids re-downloading. Thumbnails (1×, 2×) are generated asynchronously by [`thumbnails.rs`](../../src-tauri/src/thumbnails.rs).

The frontend helper `lib/tauri/artwork.ts::resolveRemoteImage` prefers the local file via `convertFileSrc` so artist imagery renders offline. The `metadata_artwork/**` scope must stay listed in `tauri.conf.json` `assetProtocol`.

## Last.fm

[`lastfm.rs`](../../src-tauri/src/lastfm.rs) — split into two flows:

### Read-only (artist bios)

`artist.getInfo` for biographies, called from `enrich_artist_deezer` after the Deezer pass. Cached in the same `deezer_artist` row (the table name is historical — it holds Last.fm bios too) with the same 30-day TTL. **Optional**: requires a user-supplied API key in `app_setting['lastfm_api_key']`. Without it, bios are skipped silently and the UI shows local data.

### Read-only (similar artists)

[`commands/similar.rs::get_similar_artists`](../../src-tauri/src/commands/similar.rs) drives the "Similar artists" carousel on `ArtistDetailView`. Cascade:

1. **Last.fm `artist.getSimilar`** when an API key is configured — returns up to 12 hits with a real 0-1 affinity score.
2. **Deezer `/artist/{id}/related`** as a fallback when Last.fm has no key, errors out, or returns an empty list. Score is synthesised from the Deezer ranking (`1.0 - i / N`) so the UI can sort uniformly across providers.

Results are cached in `app.lastfm_similar` (30-day TTL, keyed by the source artist's canonical name — same `canonical_name()` routine as the scanner). Each suggestion is augmented at query time with a `library_artist_id` when its canonical name matches a row in the active profile, so the UI can badge it as "in your library" and route the click back to the local artist page. Suggestions outside the library are rendered greyed out and non-interactive — no in-app destination exists for them yet.

### Authenticated (scrobbling)

[`scrobbler.rs`](../../src-tauri/src/scrobbler.rs) is the worker thread that drives Last.fm scrobbles:

- **Login** — signed `auth.getMobileSession` (md-5 of params + secret). Session key persisted in `app_setting['lastfm_session_key']`.
- **Now Playing** — `track.updateNowPlaying` fires on every `player:track-changed` event after the 240 s threshold. Best-effort; failures are logged but never block playback.
- **Scrobble queue** — `track.scrobble` is queued in the per-profile `scrobble_queue` table and drained with exponential backoff (10 s → 5 min). Survives app restarts.
- **Re-auth** — on `9` (`Invalid session key`) or `4` (`Authentication failed`), the session is wiped and a `lastfm:reauth` event is emitted. The frontend surfaces a banner (`LastfmReauthBanner`) with a one-click "Re-authenticate" button.

## Discord Rich Presence

[`discord_presence.rs`](../../src-tauri/src/discord_presence.rs) — speaks Discord's local IPC named pipe via the [`discord-rich-presence`](https://crates.io/crates/discord-rich-presence) crate (no network, no auth, no token). Architecture mirrors [`media_controls.rs`](../../src-tauri/src/media_controls.rs): a dedicated thread owns the `DiscordIpcClient` (which is `!Send` on Windows because it wraps a Win32 pipe handle), with a `crossbeam-channel` carrying update messages from the player code.

### Activity layout

Spotify-style card under "Listening to WaveFlow":

| Discord field | Source |
| --- | --- |
| `name` | Hard-coded `"WaveFlow"` (required by Discord — without it the IPC accepts the payload silently and nothing renders). |
| `activity_type` | `Listening` (= 2) so the header reads "Listening to WaveFlow" instead of "Playing WaveFlow". |
| `details` (line 1) | Track title. |
| `state` (line 2) | Artist (album-only fallback when the artist tag is missing — Discord requires `state` ≥ 2 chars). |
| `large_image` | Deezer cover URL when available, otherwise the `waveflow_logo` asset key. |
| `large_text` | Album title (rendered inline by Discord as line 3). |
| `small_image` / `small_text` | `play` + "En lecture" while playing, `pause` + "En pause" while paused. |
| `timestamps.start` / `.end` | Computed from track duration + current position so Discord renders the `00:42 ─── 04:30` progress bar. **Only set while `Playing`** — leaving them on while paused makes Discord keep ticking the bar from the wall clock, which lies. Re-anchored on every play / seek / pause-resume. |
| `buttons` | One button — **"Voir sur GitHub"** → `https://github.com/InstaZDLL/WaveFlow`. Clickable by anyone viewing the presence card; lets users discover the project directly from Discord. |

### Cover URL resolution

Discord propagates `large_image` URLs to other users' clients, so local files / our `127.0.0.1` artwork shim are off-limits — only public HTTPS works. [`resolve_cover_url`](../../src-tauri/src/discord_presence.rs) is two-stage:

1. **Cache hit** — `JOIN track → album → deezer_album` in the per-profile pool. Cheap, no network.
2. **Cache miss** — call `commands::deezer::enrich_album_inner` which searches Deezer by title+artist and persists the result. Subsequent plays of the same album hit stage 1.

The first track of an unenriched album takes ~1 s for the Deezer round-trip; following tracks are instant. Empty Deezer results fall back to the `waveflow_logo` asset key.

### Lifecycle

- **Default ON** — `read_enabled` returns `true` when `app_setting['integrations.discord_rpc']` is missing. Only an explicit toggle-off (writes the literal `"false"`) disables RPC. The UI toggle lives in `SettingsView` under "Intégrations".
- **Boot** — `lib.rs::setup` reads the persisted flag and spawns the worker. Discord IPC is **not** connected yet — the first connection attempt happens on the first `Msg::Metadata` after a track plays. Keeps the named pipe free when the user never plays anything.
- **Idle / Ended** — when the decoder transitions to `PlayerState::Idle` (Stop button) or `PlayerState::Ended` (queue exhausted), the worker calls `clear_activity` so the card disappears from the user's profile. Spotify-style: nothing playing → no presence.
- **Pause** — the activity stays on screen with the `pause` badge and timestamps removed; same UX as Spotify pausing in the middle of a track.
- **Discord restart** — `set_activity` failures drop the client back to `None`; the next push re-runs the handshake. No reconnect daemon needed — the next track-changed event triggers it organically.

### Assets

The Discord application (ID `1502611865698570291`) hosts three asset keys uploaded under "Rich Presence Art Assets":

- `waveflow_logo` — fallback for tracks with no Deezer cover.
- `play` — `small_image` while playing.
- `pause` — `small_image` while paused.

PNG sources live in [`assets/discord/png/`](../../assets/discord/png/), generated from the SVG sources in [`assets/discord/`](../../assets/discord/) via `bun scripts/build-discord-assets.mjs` (uses [`sharp`](https://sharp.pixelplumbing.com/) for SVG → 1024×1024 PNG conversion). Re-running the script after editing an SVG re-emits the PNGs ready to drop on the developer portal — Discord's CDN takes ~10 min to propagate updated assets.

## LRCLIB (synchronized lyrics)

[`lrclib.rs`](../../src-tauri/src/lrclib.rs) — public lookup by `artist_name + track_name + album_name + duration` against [LRCLIB](https://lrclib.net). Three-tier resolution in [`commands/lyrics.rs`](../../src-tauri/src/commands/lyrics.rs), driven on demand by `fetch_lyrics`:

1. **Cache** — `app.lyrics` row keyed by `track.file_hash` (BLAKE3). No TTL, shared across profiles.
2. **Embedded** — `LYRICS` / `USLT` / `©lyr` tag in the file (lofty), incl. synced `LRC` blocks. Lookup tries `ItemKey::UnsyncLyrics` first (the only key that maps to ID3v2's `USLT` in lofty 0.24), then `ItemKey::Lyrics` for Vorbis / MP4. For MP3s tagged with Mp3tag / foobar2000 / lame `--tg`, lyrics often live in a TXXX user-defined frame named `LYRICS` or `UNSYNCEDLYRICS` (common on K-Pop / J-Pop rips); these are invisible to the generic `Tag` interface so [`commands/lyrics.rs::read_id3v2_txxx_lyrics`](../../src-tauri/src/commands/lyrics.rs) re-opens the file as `MpegFile`, downcasts to `Id3v2Tag`, and scans the TXXX descriptions explicitly.
3. **LRCLIB** — synced lyrics first, falls back to plain text. Result cached as a new row.

In addition, `import_lrc_file` lets the user pick a `.lrc` file by hand and overwrite the cached entry — there is no automatic sidecar pickup.

**In-app editor.** `save_lyrics(track_id, { content, format, write_to_file })` upserts the cache row with `source = manual` and, when `write_to_file` is true, also writes the content into the file's `USLT` (ID3v2) / `UNSYNCEDLYRICS` (Vorbis) / `©lyr` (MP4) frame via lofty. Same Windows file-lock dance as the tag editor — pause if the engine has the file open, then re-hash with blake3 and update `track.file_hash` so the cache row stays addressable after the write. Emits a typed `lyrics:updated` event the panel listens to.

UI is [`LyricsEditorModal`](../../src/components/common/LyricsEditorModal.tsx) opened via the pencil button in the lyrics panel header. Two tabs:

- **Texte** — free-form `<textarea>` for unsynced lyrics.
- **Synchronisé** (Musicolet-style) — each row is a `(timestamp, text)` pair. A "Capturer" button (or **Space** keyboard shortcut) snaps the active row's timestamp to the player's current `positionMs`. Play / Pause / ±2 s controls pilot the existing `PlayerContext` so the user can scrub the file while writing the lines, and the rows are serialised back to LRC via [`serializeLrc`](../../src/lib/tauri/lyrics.ts).

**Cache discipline.** `clear_lyrics` flushes the row keyed by the track's hash so the next fetch re-runs the waterfall. Cached outcomes:

- Hit (embedded or LRCLIB) → row written.
- Instrumental flag from LRCLIB → empty row written (suppresses retries).
- LRCLIB 404 / empty payload → empty row written. Without this, lo-fi / ambient libraries would re-hit the network on every panel open since most of their tracks are genuinely missing from LRCLIB. The lyrics panel renders "no lyrics found" against an empty cached row, and the "Refetch" button (`clearLyrics + fetchLyrics`) is the manual escape hatch for the user to retry once they think LRCLIB might have added the track.
- Network error → **not cached** and bubbled up to the UI as `Err` so the panel can show "retry" instead of a misleading "no lyrics" state.

**Network defaults.** 15 s overall timeout + 5 s connect-timeout in `LrclibClient` so a slow LRCLIB instance still gets a chance to respond while a truly unreachable host fails fast.

**Library-wide prefetch.** `prefetch_library_lyrics` walks every available track without a cached row (deduped by `file_hash`), runs the embedded → LRCLIB chain, and persists each hit. Network calls are throttled at 500 ms (~2 req/s) to be a polite guest; embedded hits skip the throttle. Progress streams over `lyrics:prefetch-progress`. A single global run is enforced via an `AtomicBool`; `cancel_lyrics_prefetch` flips a second `AtomicBool` the worker checks per iteration. Resumable — a partial cancel just leaves uncached rows for the next run.

The lyrics panel renders synced lines with auto-scroll and a 200 ms transition; un-synced lyrics fall back to a static block.
