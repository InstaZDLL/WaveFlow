# Integrations

External services WaveFlow talks to. All clients use [`reqwest 0.12`](https://crates.io/crates/reqwest) with `rustls-tls` so there's no system OpenSSL dependency.

## Offline mode

A single global toggle — Settings → Intégrations → "Mode hors-ligne" — short-circuits every outbound call described below. The flag is a `static AtomicBool` in [`offline.rs`](../../src-tauri/crates/app/src/offline.rs); it is consulted by Deezer enrichment, Last.fm now-playing + the scrobble worker tick, similar-artist lookups, and lyrics fetch + library prefetch providers. Each gated path returns an empty payload (or whatever the local cache holds), nothing throws, so the UI keeps rendering with whatever metadata is already on disk. Persisted in `app_setting['network.offline_mode']` because the flag is process-wide — switching profiles must not silently re-enable network calls.

## Deezer (metadata)

[`deezer.rs`](../../src-tauri/crates/core/src/metadata/deezer.rs) — public Deezer API, no auth. Used for:

- Artist pictures (`enrich_artist_deezer`, `batch_fetch_missing_artist_pictures`)
- Album covers (`enrich_album_deezer`, `search_albums_deezer`, `set_album_artwork_from_deezer`, `batch_fetch_missing_album_covers`)
- Label / fan-count metadata

Results are cached in the `deezer_artist` / `deezer_album` tables of the **shared** `app.db` (one cache across every profile) with a 30-day `expires_at` TTL. Cache-first: zero network round-trips when the row is fresh. Failures are non-fatal — the UI degrades to local-only artwork and an empty enrichment payload.

**Auto-enrichment on play.** [`PlayerProvider`](../../src/contexts/PlayerContext.tsx) fires `enrich_artist_deezer(currentTrack.artist_id)` (fire-and-forget) on every track-change. Cache hits are ~10 ms so the duplicate call done by `NowPlayingPanel` when it renders is harmless; the point is to populate the cache for views the user _isn't_ looking at right now (e.g. the artist grid in `LibraryView`) so a tile gets its picture as soon as the user plays one of that artist's tracks, regardless of whether the Now Playing panel is open.

**Batch fill-in.** `batch_fetch_missing_artist_pictures` walks every artist with no cached row (or an expired one), runs the standard enrichment per artist, and emits `artist-fetch-progress` so a Settings progress bar can drive the UI. Throttled at 200 ms (~5 req/s) to stay well below Deezer's anonymous rate limit. Same idempotent semantics as `batch_fetch_missing_album_covers`: re-running just resumes on whatever's still missing.

Downloaded images go through [`metadata_artwork::download_and_cache`](../../src-tauri/crates/core/src/artwork/metadata.rs): Blake3-hashed bytes → `<root>/metadata_artwork/<hash>.jpg`. The hash is persisted in `deezer_artist.picture_hash` / `deezer_album.cover_hash` so a cache hit on the metadata table avoids re-downloading. Thumbnails (1×, 2×) are generated asynchronously by [`thumbnails.rs`](../../src-tauri/crates/core/src/artwork/thumbnails.rs).

The frontend helper `lib/tauri/artwork.ts::resolveRemoteImage` prefers the local file via `convertFileSrc` so artist imagery renders offline. The `metadata_artwork/**` scope must stay listed in `tauri.conf.json` `assetProtocol`.

## Last.fm

[`lastfm.rs`](../../src-tauri/crates/core/src/metadata/lastfm.rs) — split into two flows:

### Read-only (artist bios)

`artist.getInfo` for biographies, called from `enrich_artist_deezer` after the Deezer pass. Cached in the same `metadata_artist` row with the same 30-day TTL. **Optional**: requires a user-supplied API key in `app_setting['lastfm_api_key']`. Without it, bios are skipped silently and the UI shows local data.

**Bio source selector (issue #295).** The bio provider is switchable in Settings → Integrations between **Last.fm** (default — English, needs the key) and **TheAudioDB** ([`theaudiodb.rs`](../../src-tauri/crates/core/src/metadata/theaudiodb.rs) — community DB, multi-language, no key; free shared API key `123`, 30 req/min). The choice lives in `app_setting['metadata.bio_source']` and, for TheAudioDB, a language in `app_setting['metadata.bio_language']` (the client maps `strBiography{LANG}` and falls back to English). `enrich_artist_deezer` branches on the active source and stores `bio_source` / `bio_language` alongside the bio in `metadata_artist`; the cache check treats the bio as stale (re-fetches) when either differs from the active setting, so switching source or language refreshes on the next view. Like Last.fm, the bio still attaches to the Deezer-keyed cache row, so it only resolves for artists that match on Deezer.

### Read-only (similar artists)

[`commands/similar.rs::get_similar_artists`](../../src-tauri/crates/app/src/commands/similar.rs) drives the "Similar artists" carousel on `ArtistDetailView`. Cascade:

1. **Last.fm `artist.getSimilar`** when an API key is configured — returns up to 12 hits with a real 0-1 affinity score.
2. **Deezer `/artist/{id}/related`** as a fallback when Last.fm has no key, errors out, or returns an empty list. Score is synthesised from the Deezer ranking (`1.0 - i / N`) so the UI can sort uniformly across providers.

Results are cached in `app.lastfm_similar` (30-day TTL, keyed by the source artist's canonical name — same `canonical_name()` routine as the scanner). Each suggestion is augmented at query time with a `library_artist_id` when its canonical name matches a row in the active profile, so the UI can badge it as "in your library" and route the click back to the local artist page. Suggestions outside the library are rendered greyed out and non-interactive — no in-app destination exists for them yet.

**Picture enrichment.** Last.fm's `artist.getSimilar` returns the same generic star placeholder URL for every result (their artist-image endpoint was retired in 2019). To avoid a sea of grey stars when the cascade picks the Last.fm branch, `get_similar_artists` runs the raw list through `enrich_with_deezer_pictures` before responding: it pulls every non-expired row from the cross-profile `app.metadata_artist` cache in a single SELECT, then filters in Rust against `canonical_name(&row.name)` so artists with punctuation (e.g. AC/DC, P!nk) match correctly — SQLite's standard build has no REGEXP function so a `LOWER(TRIM(name))` predicate would mismatch the scanner's alphanumeric canonicalisation. Cache misses fan out to Deezer `search_artist` through a `futures::stream::buffer_unordered(CONCURRENCY_LIMIT)` bounded at 12, and the miss set itself is trimmed to `RESULT_LIMIT` so we never burn network on entries the caller's `.take(RESULT_LIMIT)` will drop. New rows are upserted back so the picture survives for 30 days. The DTO's `picture_url` is rewritten to the Deezer URL whenever one is available; `picture_path` prefers the in-library hash (set by the existing profile-DB join) before falling back to the freshly cached `metadata_artist.picture_hash`. When offline mode is on the function reads the cache **without** the `expires_at > now` predicate (we have no way to refresh anyway, and the `metadata_artwork/<hash>.jpg` blob never expires — serving a stale picture beats showing a grey star) and then short-circuits before the network refresh. DB errors on both the cache read and the upsert are logged via `tracing::warn!` and degrade to "no enrichment" — never block the response.

### Manual overrides (offline-first, issue #323)

Both the bio and the similar list can be **manually overridden per-artist**, mirroring the existing local `artist.jpg` picture sidecar — for long-tail repertoires where Deezer/Last.fm metadata is sparse, and for offline-mode users who otherwise get nothing. Edited from **Artist Detail → "Edit info"** ([`ArtistMetadataEditorModal`](../../src/components/common/ArtistMetadataEditorModal.tsx)), persisted **per-profile** in the profile DB (migration `20260628000000_artist_metadata_overrides`):

- **Bio** — free text in `artist.custom_bio`. [`enrich_artist_deezer`](../../src-tauri/crates/app/src/commands/deezer.rs) reads it up-front and swaps it onto whatever the enrichment path returns (cache hit, offline short-circuit, or fresh fetch). The inner path still fetches + caches the online bio so the **shared** cross-profile `metadata_artist` cache stays correct for profiles without an override — only the returned value is swapped. Clearing the field (blank → stored `NULL`) drops the override.
- **Similar** — library-scoped, user-curated rows in `artist_similar_custom (artist_id, similar_artist_id, position)`. [`get_similar_artists`](../../src-tauri/crates/app/src/commands/similar.rs) short-circuits to this list (source `"custom"`) before any cache/network, so it works fully offline; every entry is in the library by construction. Picked via the topbar `search_artists` autocomplete. An empty list drops the override and the online cascade takes back over.

Write commands: [`set_artist_bio_override`](../../src-tauri/crates/app/src/commands/artist_overrides.rs) + `set_artist_similar_override` + `get_artist_overrides` (pre-fills the editor). Overrides survive enrichment passes because the pull paths write the shared `app.metadata_artist` cache, never the per-profile `artist` row / override table.

### Authenticated (scrobbling)

[`scrobbler.rs`](../../src-tauri/crates/app/src/scrobbler.rs) is the worker thread that drives Last.fm scrobbles:

- **Login** — signed `auth.getMobileSession` (md-5 of params + secret). Session key persisted in `app_setting['lastfm_session_key']`.
- **Now Playing** — `track.updateNowPlaying` fires on every `player:track-changed` event after the 240 s threshold. Best-effort; failures are logged but never block playback.
- **Scrobble queue** — `track.scrobble` is queued in the per-profile `scrobble_queue` table and drained with exponential backoff (10 s → 5 min). Survives app restarts.
- **Re-auth** — on `9` (`Invalid session key`) or `4` (`Authentication failed`), the session is wiped and a `lastfm:reauth` event is emitted. The frontend surfaces a banner (`LastfmReauthBanner`) with a one-click "Re-authenticate" button.

## Discord Rich Presence

[`discord_presence.rs`](../../src-tauri/crates/app/src/discord_presence.rs) — speaks Discord's local IPC named pipe via the [`discord-rich-presence`](https://crates.io/crates/discord-rich-presence) crate (no network, no auth, no token). Architecture mirrors [`media_controls.rs`](../../src-tauri/crates/app/src/media_controls.rs): a dedicated thread owns the `DiscordIpcClient` (which is `!Send` on Windows because it wraps a Win32 pipe handle), with a `crossbeam-channel` carrying update messages from the player code.

### Activity layout

Spotify-style card under "Listening to WaveFlow":

| Discord field                | Source                                                                                                                                                                                                                                                                                  |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `name`                       | Hard-coded `"WaveFlow"` (required by Discord — without it the IPC accepts the payload silently and nothing renders).                                                                                                                                                                    |
| `activity_type`              | `Listening` (= 2) so the header reads "Listening to WaveFlow" instead of "Playing WaveFlow".                                                                                                                                                                                            |
| `details` (line 1)           | Track title.                                                                                                                                                                                                                                                                            |
| `state` (line 2)             | Artist (album-only fallback when the artist tag is missing — Discord requires `state` ≥ 2 chars).                                                                                                                                                                                       |
| `large_image`                | Deezer cover URL when available, otherwise the `waveflow_logo` asset key.                                                                                                                                                                                                               |
| `large_text`                 | Album title (rendered inline by Discord as line 3).                                                                                                                                                                                                                                     |
| `small_image` / `small_text` | `play` + "En lecture" while playing, `pause` + "En pause" while paused.                                                                                                                                                                                                                 |
| `timestamps.start` / `.end`  | Computed from track duration + current position so Discord renders the `00:42 ─── 04:30` progress bar. **Only set while `Playing`** — leaving them on while paused makes Discord keep ticking the bar from the wall clock, which lies. Re-anchored on every play / seek / pause-resume. |
| `buttons`                    | One button — **"Voir sur GitHub"** → `https://github.com/InstaZDLL/WaveFlow`. Clickable by anyone viewing the presence card; lets users discover the project directly from Discord.                                                                                                     |

### Cover URL resolution

Discord propagates `large_image` URLs to other users' clients, so local files / our `127.0.0.1` artwork shim are off-limits — only public HTTPS works. [`resolve_cover_url`](../../src-tauri/crates/app/src/discord_presence.rs) is two-stage:

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

## Native OS notifications

[`notifications.rs`](../../src-tauri/crates/app/src/notifications.rs) — fires a single track-change toast via `tauri-plugin-notification`. Different axis from [`media_controls.rs`](../../src-tauri/crates/app/src/media_controls.rs): SMTC / MPRIS / MediaRemote drive the **OS media overlay** (lock screen, volume flyout, Now Playing widget), while a notification is a **transient toast**. Both can coexist and most desktop players ship both.

Triggered from [`emit_track_changed`](../../src-tauri/crates/app/src/commands/player.rs) in a tokio task so the SQLite opt-in lookup doesn't sit on the path that flips the player-bar metadata. The notification carries only the track title + artist (no cover image) — Windows Action Center, KDE / GNOME notification daemons, and macOS Notification Center all support an icon slot but they expect a URL or file path the OS can read, and we'd need a fourth path next to SMTC's `127.0.0.1` shim + Discord's public Deezer URL to feed it cleanly. Title + artist is the format every system handles uniformly.

**Off by default** — opposite default from Discord RPC because toasts are intrusive and trigger Focus Assist (Windows), Do Not Disturb (macOS), and `org.freedesktop.Notifications` filters (Linux) on every platform. Opt-in via Settings → Intégrations → "Notifications de changement de morceau". Stored in `app_setting['notifications.track_change']` (typed `bool`, shared across profiles like Discord RPC since toasts are an OS-level user preference, not per-listener). Toggling on doesn't fire a toast for the current track — first toast lands on the **next** track change.

## Lyrics providers

[`lrclib.rs`](../../src-tauri/crates/core/src/metadata/lrclib.rs) still handles the exact [LRCLIB](https://lrclib.net) lookup by `artist_name + track_name + album_name + duration`. Query-based providers live in the Tauri-free [`waveflow-syncedlyrics`](../../src-tauri/crates/syncedlyrics/src/lib.rs) crate and are called from [`commands/lyrics.rs`](../../src-tauri/crates/app/src/commands/lyrics.rs). `fetch_lyrics` and the library-wide `prefetch_library_lyrics` walk the same waterfall:

1. **Cache** — `app.lyrics` row keyed by `track.file_hash` (BLAKE3). No TTL, shared across profiles.
2. **Embedded** — `LYRICS` / `USLT` / `©lyr` tag in the file (lofty), incl. synced `LRC` blocks. Lookup tries `ItemKey::UnsyncLyrics` first (the only key that maps to ID3v2's `USLT` in lofty 0.24), then `ItemKey::Lyrics` for Vorbis / MP4. For MP3s tagged with Mp3tag / foobar2000 / lame `--tg`, lyrics often live in a TXXX user-defined frame named `LYRICS` or `UNSYNCEDLYRICS` (common on K-Pop / J-Pop rips); these are invisible to the generic `Tag` interface so [`commands/lyrics.rs::read_id3v2_txxx_lyrics`](../../src-tauri/crates/app/src/commands/lyrics.rs) re-opens the file as `MpegFile`, downcasts to `Id3v2Tag`, and scans the TXXX descriptions explicitly.
3. **Sidecar file** — `{stem}.lrc` / `{stem}.txt` next to the audio file (e.g. `01 Song.mp3` + `01 Song.lrc`), or inside a sibling `Lyrics/` folder (case-insensitive, so `lyrics/` is also matched — common Linux convention). Stem matching is also case-insensitive so `Song.MP3` finds `song.lrc` on case-sensitive filesystems. `.lrc` wins over `.txt` at every probed directory because it carries timing info; same-folder hits beat `Lyrics/` hits. Format is auto-detected via `detect_format` and the row is cached with `source = lrc_file`. Whitespace-only files are treated as misses so the waterfall keeps falling through. `save_lyrics` still writes back only to the embedded tag — the sidecar file remains read-only — so a user who edits via the in-app editor sees the new content immediately (from the freshly-hashed cache row), and the old sidecar copy is silently superseded by the new embedded tag on subsequent re-hashes.
4. **Musixmatch Enhanced** — asks for word-level karaoke first. It only wins early when the result is actually Enhanced LRC; regular line-level LRC from Musixmatch falls through so LRCLIB's stricter metadata match can still win.
5. **LRCLIB** — synced lyrics first, falls back to plain text. Result cached as a new row.
6. **Query-based fallback providers** — Musixmatch, NetEase, Megalobiz, then Genius. This broader scan only runs after LRCLIB returns 404 or an empty payload, and prefers synced content over plain text.

`import_lrc_file` is still available for the explicit "pick this file" flow (e.g. when the sidecar lives in a non-conventional location like `~/Documents/lyrics/`); it overwrites the cached row regardless of which tier filled it.

**In-app editor.** `save_lyrics(track_id, { content, format, write_to_file })` upserts the cache row with `source = manual` and, when `write_to_file` is true, also writes the content into the file's `USLT` (ID3v2) / `UNSYNCEDLYRICS` (Vorbis) / `©lyr` (MP4) frame via lofty. Same Windows file-lock dance as the tag editor — pause if the engine has the file open, then re-hash with blake3 and update `track.file_hash` so the cache row stays addressable after the write. Emits a typed `lyrics:updated` event the panel listens to.

UI is [`LyricsEditorModal`](../../src/components/common/LyricsEditorModal.tsx) opened via the pencil button in the lyrics panel header. Two tabs:

- **Texte** — free-form `<textarea>` for unsynced lyrics.
- **Synchronisé** (Musicolet-style) — each row is a `(timestamp, text)` pair. A "Capturer" button (or **Space** keyboard shortcut) snaps the active row's timestamp to the player's current `positionMs`. Play / Pause / ±2 s controls pilot the existing `PlayerContext` so the user can scrub the file while writing the lines, and the rows are serialised back to LRC via [`serializeLrc`](../../src/lib/tauri/lyrics.ts).

**Cache discipline.** `clear_lyrics` flushes the row keyed by the track's hash so the next fetch re-runs the waterfall. Cached outcomes:

- Hit (embedded, sidecar, LRCLIB, or query provider) → row written.
- Instrumental flag from LRCLIB → empty row written (suppresses retries).
- LRCLIB 404 / empty payload and every query provider misses → empty row written. Without this, lo-fi / ambient libraries would re-hit the network on every panel open since many of their tracks are genuinely missing from public lyric providers. The lyrics panel renders "no lyrics found" against an empty cached row, and the "Refetch" button (`clearLyrics + fetchLyrics`) is the manual escape hatch for the user to retry once they think providers might have added the track.
- Network error → **not cached** and bubbled up to the UI as `Err` so the panel can show "retry" instead of a misleading "no lyrics" state.

**Network defaults.** 15 s overall timeout + 5 s connect-timeout in both `LrclibClient` and `SyncedLyricsClient` so a slow provider still gets a chance to respond while a truly unreachable host fails fast. Genius and NetEase can receive cookies through `SYNCEDLYRICS_GENIUS_COOKIE` and `SYNCEDLYRICS_NETEASE_COOKIE` for deployments that need them; secrets stay in the environment, never in the database.

**Library-wide prefetch.** `prefetch_library_lyrics` walks every available track without a cached row (deduped by `file_hash`), runs the same waterfall, and persists each hit. Network calls are throttled at 500 ms (~2 req/s) to be a polite guest; embedded and sidecar hits skip the throttle. Progress streams over `lyrics:prefetch-progress`. A single global run is enforced via an `AtomicBool`; `cancel_lyrics_prefetch` flips a second `AtomicBool` the worker checks per iteration. Resumable — a partial cancel just leaves uncached rows for the next run.

The lyrics panel renders synced lines with auto-scroll and a 200 ms transition; un-synced lyrics fall back to a static block.

### Word-level lyrics (Enhanced LRC + TTML)

WaveFlow recognises two word-timed formats in addition to plain LRC:

- **Enhanced LRC** — `[mm:ss.xx]La <mm:ss.xx>nuit <mm:ss.xx>tombe`. Plain-text extension of the LRC ecosystem; round-trips cleanly through `USLT` so other players see it as regular synced LRC if they don't parse the inline word stamps.
- **TTML** (Apple Music) — XML envelope with `<p begin="…" end="…"><span begin="…" end="…">word</span></p>`. Imported from `.ttml` / `.xml` files exported by tools like LyricsX. Char-level spans nested inside word spans are folded into their parent — v1 ships with word-level animation only.

**Detection** — [`commands/lyrics.rs::detect_format`](../../src-tauri/crates/app/src/commands/lyrics.rs) sniffs the cached content. TTML matches first on `<?xml`, `<tt`, or the `http://www.w3.org/ns/ttml` namespace. Enhanced LRC requires both a `[mm:ss…]` line stamp and at least one `<mm:ss…>` word stamp inside the line body; falling back to plain LRC otherwise. The same heuristic runs on the editor's save path so user-typed content gets re-classified if they switch between modes.

**Storage** — `app.lyrics.format` accepts the new `'ttml'` value via [migration 20260516120000_lyrics_ttml_format.sql](../../src-tauri/migrations/app/20260516120000_lyrics_ttml_format.sql) (CHECK rebuild — SQLite has no ALTER CONSTRAINT). The `content` column stays raw text — there's no separate `words` column; parsing is done at render time on the frontend. This keeps the cache byte-for-byte identical to what would be written into the tag and avoids a hot migration over user data.

**Parsing** — `src/lib/tauri/lyrics.ts` exposes `parseLrc`, `parseEnhancedLrc`, `parseTtml`, and a unifying `parseLyrics(content, format)` dispatcher. All three return the same `LyricsLine` shape (`timeMs`, `endMs`, `text`, optional `words[]`). The TTML parser uses the webview's built-in `DOMParser` — no XML dependency. `findActiveWordIndex` mirrors `findActiveLineIndex` (linear scan from hint, O(1) amortised).

**Rendering** — [`LyricsPanel`](../../src/components/layout/LyricsPanel.tsx) and [`ImmersiveLyricsColumn`](../../src/components/player/ImmersiveLyricsColumn.tsx) share the same active-word animation: 150 ms transitions on color / opacity / transform, `scale(1.04)` on the active word, and a 0.45 → 0.8 → 1 opacity ramp for future / past / active words. The panel adds an accent-color tint that the fullscreen view leaves out (the white-on-dark contrast is enough there). Lines without `words` keep the existing line-level highlight.

**Editor — word mode.** [`LyricsEditorModal`](../../src/components/common/LyricsEditorModal.tsx) adds a granularity toggle inside the synchronized tab. In word mode:

- **Space** — stamps the next un-captured word in the active line. First press also stamps the line's own `timeMs` if it's not yet captured.
- **Enter** — advances to the next line (appending a fresh empty one at the end, like line mode).
- **Backspace** — undoes the last word capture on the active line.

The row UI shows each word as a chip — pink for captured, green-ringed for the next word to capture, grey for future words. Editing a line's text invalidates its word tokenisation, so the user has to re-capture cleanly. The save path serialises back to Enhanced LRC via `serializeEnhancedLrc` regardless of the originally-imported format (TTML round-trip isn't part of v1).

**TTML → USLT.** The audio file's `USLT` frame is plain-text by spec, so writing TTML into it would corrupt other players. `write_lyrics_to_file` therefore:

- Plain / LRC / Enhanced LRC → `ItemKey::UnsyncLyrics` (USLT for ID3v2, UNSYNCEDLYRICS for Vorbis, `©lyr` for MP4) — unchanged.
- TTML on Vorbis / MP4 / FLAC → `ItemKey::Lyrics` (the XML-friendly key).
- TTML on MP3 — **skipped**. lofty has no clean ID3v2 mapping for arbitrary XML lyrics, so the file is left untouched, the DB cache still gets the TTML content, and `save_lyrics` returns `tag_write_skipped: true`. The editor surfaces this as a `lyrics.toast.tagWriteSkipped` warning so the user knows the file itself wasn't touched.
