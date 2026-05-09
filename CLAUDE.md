# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

WaveFlow is a local music player desktop app built with **Tauri 2 + React 19 + TypeScript + Vite**. It uses a Spotify/Apple Music-inspired UI for browsing and playing local audio files. The project uses **bun** as the package manager.

For per-feature deep dives (algorithms, schema, flow diagrams) read the relevant page under [`docs/`](docs/README.md) — that's the source of truth when this file's overview isn't enough. This `CLAUDE.md` only covers the cross-cutting patterns Claude needs in every conversation.

## Development Commands

```bash
# Install dependencies
bun install

# Run the Tauri desktop app in development mode (starts Vite dev server + Rust backend)
bun run tauri dev

# Build the production desktop app
bun run tauri build

# Run only the Vite frontend dev server (no Tauri shell)
bun run dev

# TypeScript type check
bun run typecheck

# Lint
bun run lint

# TypeScript check + Vite production build (frontend only)
bun run build

# Rust backend
cargo check --manifest-path src-tauri/Cargo.toml --all-targets
cargo test  --manifest-path src-tauri/Cargo.toml
```

## Architecture

### Frontend (`src/`)

React 19 + TypeScript. Entry point: `src/main.tsx` → `src/App.tsx`. Vite dev server on port 1420.

- **Contexts**: `ThemeContext`, `PlayerContext`, `LibraryContext`, `PlaylistContext`, `ProfileContext` — mounted in `App.tsx` as a provider tree. `PageScrollContext` is mounted lower (inside `AppLayout`) and exposes the main scrollable area's ref to virtualized tables so the page drives a single scrollbar.
- **Hooks**: `useTheme`, `usePlayer`, `useLibrary`, `usePlaylist`, `useProfile`, `usePageScroll` — each wraps its context.
- **Tauri wrappers** (`src/lib/tauri/`): typed `invoke()` wrappers for every backend command (`track.ts`, `browse.ts`, `player.ts`, `playlist.ts`, `library.ts`, `detail.ts`, `integration.ts`, `lyrics.ts`, `stats.ts`, `artwork.ts`, `analysis.ts`, `dialog.ts`, `deezer.ts`, `profile.ts`). All commands are camelCase on the frontend, snake_case on the backend.
- **Views**: `HomeView`, `LibraryView`, `PlaylistView`, `AlbumDetailView`, `ArtistDetailView`, `LikedView`, `RecentView`, `StatisticsView`, `SettingsView`, etc.
- **Common components**: `ArtistLink` (per-name clickable multi-artist renderer), `Artwork` (file-scoped asset protocol resolver), playlist visuals.
- **Layout**: Apple Music-style sidebar (Ma musique sub-navs + Playlists section), TopBar with search, PlayerBar at bottom, two right-edge panels (`QueuePanel` + `NowPlayingPanel`) mutually exclusive via `PlayerContext`.

### Backend (`src-tauri/`)

Rust/Tauri 2. Entry point: `src-tauri/src/main.rs` → `lib.rs`.

- **Commands** (`src-tauri/src/commands/`): organized by domain — `library.rs`, `playlist.rs` (CRUD + M3U import/export), `smart_playlists.rs` (Daily Mix regen entry point), `track.rs`, `browse.rs`, `player.rs` (playback + output device list/select), `scan.rs`, `profile.rs`, `analysis.rs` (peak/loudness/ReplayGain/BPM), `deezer.rs` (metadata enrichment + batch fill-in for missing artist pictures and album covers), `similar.rs` (similar-artist discovery, Last.fm primary + Deezer `/related` fallback, cached in `app.lastfm_similar`), `lyrics.rs` (LRCLIB + embedded fallback + .lrc import + library-wide prefetch), `stats.rs` (listening analytics from `play_event`), `integration.rs` (Last.fm API key storage), `maintenance.rs`, `app_info.rs`. All registered in `lib.rs` via `generate_handler![]`.
- **External API clients** (crate-root modules): `deezer.rs` (public Deezer API, no auth) and `lastfm.rs` (Last.fm `artist.getInfo`, requires user-provided API key). Both use `reqwest` with `rustls-tls`.
- **DLNA / UPnP MediaServer** ([`src-tauri/src/dlna/`](src-tauri/src/dlna/)): worker thread owning a tokio runtime + an axum HTTP server + an SSDP announcer/responder on `239.255.255.250:1900`. Exposes the active profile as `urn:schemas-upnp-org:device:MediaServer:1` so Yamaha / Sonos / Kodi / VLC discover and stream the library over the LAN. Object IDs are string prefixes (`0/artists/<id>`, `0/albums/<id>`, `0/track/<id>`); ContentDirectory `Browse` paginates via `LIMIT/OFFSET` capped at 500. `/stream/<id>` serves audio with HTTP `Range` (`206 Partial Content` + DLNA hints). Opt-in flag `app_setting['dlna.enabled']`, default OFF. Persisted config (`server_name`, `port`) lives in `app_setting` because the server is process-wide, not per-profile. See [`docs/features/dlna.md`](docs/features/dlna.md).
- **Discord Rich Presence** (`src-tauri/src/discord_presence.rs`): named-pipe IPC bridge to the local Discord client via the `discord-rich-presence` crate. Same dedicated-thread + crossbeam-channel pattern as `media_controls.rs`. Spotify-style "Listening to WaveFlow" card (title / artist / album + cover from Deezer + progress bar). Opt-in flag in `app_setting['integrations.discord_rpc']`, **default ON**. See [`docs/features/integrations.md`](docs/features/integrations.md#discord-rich-presence).
- **OS media controls** (`src-tauri/src/media_controls.rs`): souvlaki bridge wired to SMTC / MPRIS / MediaRemote. Initialized after the main window exists (needs HWND on Windows). Now Playing artwork is served to SMTC over a tiny localhost HTTP shim because Windows expects a URL, not a file path.
- **Audio engine** (`src-tauri/src/audio/`): 3-thread lock-free architecture:
  - `engine.rs` — `AudioCmd` enum, `AudioEngine` handle
  - `decoder.rs` — symphonia decode loop, rubato resampling
  - `output.rs` — cpal callback on dedicated thread, SPSC ring buffer (rtrb), output-device enumeration (ALSA hints on Linux, `cpal` elsewhere)
  - `state.rs` — `SharedPlayback` with atomics (no locks in hot path)
  - `analytics.rs` — tokio task for play_event writes + auto-advance
  - `crossfade.rs` — dual-decoder mix with equal-power gain curves over the user-set window. `ActiveStream` carries a `StreamBackend` enum (`Symphonia` / `Dsd`) so the seek + reset paths are uniform across formats.
  - `dsd/` — DSF + DFF parsers, in-house DSD-to-PCM converter (256-tap Blackman-Harris FIR, decimation 64 → DSD64 lands at 44.1 kHz), and a metadata reader (DSF carries an ID3v2 blob in its footer; DFF uses native DIIN/COMT chunks). Symphonia 0.5 doesn't decode DSD natively; this is the entire DSD playback path.
- **Queue** (`src-tauri/src/queue.rs`): persistent queue with fill, advance, shuffle (Fisher-Yates), restore.
- **Smart playlists** (`src-tauri/src/smart_playlists/`): auto-generated Daily Mix family. `generator.rs` clusters top artists by tempo (BPM from `track_analysis`) and materializes `is_smart = 1` rows in the regular `playlist` table; `cover.rs` renders a composite from up to 3 cached Deezer artist pictures into the shared `metadata_artwork/<hash>.jpg` cache. Idempotent — re-running rewrites the same slot via `LIKE '%"slot":N%'` on `smart_rules`. See [`docs/features/smart-playlists.md`](docs/features/smart-playlists.md).
- **Database**: per-profile SQLite via sqlx + a global `app.db` for profile list and app-wide settings (`app_setting` table — including the Last.fm API key). Migrations under `migrations/profile/` are append-only and applied at boot. FTS5 contentless for search with auto-sync triggers using the `'delete'` command.

### Key Patterns

- **Tauri commands**: `#[tauri::command]` in `commands/*.rs`, registered in `lib.rs` `generate_handler![]`, called from React with `invoke("command_name", { args })`.
- **Profile-scoped pool**: `state.require_profile_pool().await?` — every command that touches user data goes through this.
- **Persistence**: settings stored in `profile_setting` table (key-value with typed values). Pattern: `INSERT ... ON CONFLICT DO UPDATE`.
- **Events**: backend emits Tauri events (`player:state`, `player:position`, `player:track-changed`, `player:queue-changed`, `player:error`). Frontend listens via `listen()` from `@tauri-apps/api/event`.
- **Audio callback constraints**: the cpal callback MUST NOT allocate, lock, or log. Only `rtrb::Consumer` + `Atomic*` reads.
- **Virtual scroll**: TrackTable uses `@tanstack/react-virtual` for 6000+ track performance.
- **Multi-artist**: the scanner splits `"A, B"` on `", " / "; "` into individual `artist` rows linked via the `track_artist` many-to-many table. Queries rebuild the display string via `GROUP_CONCAT` over `track_artist` ordered by `position`. `ArtistLink` accepts parallel `artist_name` + `artist_ids` strings so every contributor is individually clickable.
- **Single-instance lock**: [`tauri-plugin-single-instance`](https://crates.io/crates/tauri-plugin-single-instance) is wired as the **first** plugin in [`lib.rs`](src-tauri/src/lib.rs) so a duplicate launch exits cleanly before any heavy init (pool open, audio engine, tray, watchers) runs in the second process. The handler shows / unminimizes / focuses the existing main window, matching the behaviour of Spotify and most desktop music players. No new IPC command — invisible to the frontend.
- **Folder cover fallback**: when a track has no embedded picture, [`scan.rs::extract_folder_cover`](src-tauri/src/commands/scan.rs) probes the parent directory for `cover|folder|front|albumart|album|artwork.{jpg,jpeg,png,webp,bmp,gif,tiff}` (stem priority, not alphabetical), hashes the bytes with blake3 and stores them in the same `artwork/` dir with `source = 'folder'`. `upsert_artwork` takes the source label as a parameter so embedded / folder / Deezer / user provenance stays queryable for future cleanup jobs.
- **Metadata cache**: Deezer (pictures, fans) and Last.fm (bios) results are cached in the `deezer_artist` / `deezer_album` tables stored in `app.db` (shared across profiles) with a 30-day `expires_at` TTL. Cache-first flow in `enrich_artist_deezer` — zero network if the row is fresh. Failures are non-fatal (empty enrichment returned, UI shows local data). The Last.fm API key is read from `app_setting` via `integration::read_lastfm_api_key` and is optional: without it, bios are skipped silently.
- **Remote artwork on disk**: Deezer pictures/covers are downloaded into the shared `<root>/metadata_artwork/<blake3>.jpg` cache by `metadata_artwork::download_and_cache`. The blake3 hash is persisted in `deezer_artist.picture_hash` / `deezer_album.cover_hash` (Deezer always serves JPEG, hence the hardcoded extension). Enrichment commands and `list_artists` / `get_artist_detail` / `stats_top_artists` return both `picture_url` (remote fallback) and `picture_path` (absolute local path); the frontend helper `lib/tauri/artwork.ts::resolveRemoteImage` prefers the local file via `convertFileSrc` so artist imagery renders offline. The `metadata_artwork/**` scope must stay listed in `tauri.conf.json` `assetProtocol`.
- **Output device persistence**: the chosen device's `name` is stored in `profile_setting['audio.output_device']`. `lib.rs` reads it during `setup` and forwards it to the audio engine, so playback resumes on the user's preferred sink without waiting for the frontend to settle.
- **First-run onboarding**: `AppLayout` latches a single decision per profile via `profile_setting['onboarding.dismissed']`. The modal only appears when the profile is fully resolved AND the library is empty AND the flag isn't set — preventing flashes during boot transitions.
- **Page-level scrolling**: virtualized tables in `PlaylistView` / `LibraryView` consume `usePageScroll()` for the scroll element instead of nesting their own `overflow-y-auto`. They compute `scrollMargin` from the parent's offset within the page scroller so `useVirtualizer` knows where their content begins. Drives a single Spotify-style scrollbar.
- **Right panels are flex siblings, not overlays**: `NowPlayingPanel` / `QueuePanel` / `LyricsPanel` are mounted as flex children of the outer row in `AppLayout`, not as `absolute` overlays inside the center column. The center column has `min-w-0` so wide tables collapse instead of pushing the panel off-screen — Spotify-style responsive shrink instead of overlap. `DeviceMenu` and `NowPlayingChevronTab` stay inside the center column so their `right-0` anchors to the content edge.
- **Output device naming on cpal 0.17**: `device.description().name()` returns Windows' generic `DEVPKEY_Device_DeviceDesc` (literally `"Speakers"` for every speaker-class endpoint). [`audio/output.rs::device_display_name`](src-tauri/src/audio/output.rs) prefers `description().extended()[0]` (the disambiguated `DEVPKEY_Device_FriendlyName`) when available — required so multiple endpoints in the same class stay distinguishable in the picker AND so `profile_setting['audio.output_device']` matches a unique device on next boot.
- **Smart playlist covers**: `playlist.cover_hash` (added in migration `20260509000000`) points into the shared `metadata_artwork/<hash>.jpg` cache. `list_playlists` / `get_playlist` derive `cover_path` post-fetch via `metadata_artwork::existing_path` so a stale hash (cache wiped) doesn't render a broken image. The "Daily Mix N" label is rendered in HTML/CSS over the JPEG by the frontend — text is intentionally not rasterised in Rust to avoid a font dep and let translations / restyles regenerate without a re-render pass.
- **User playlist auto-covers**: `playlist.cover_is_auto` (added in migration `20260509100000`) tells the post-mutation hook (`commands::playlist_cover::maybe_regen_auto_cover`, called by every add/remove/reorder/source/import command) whether to refresh the cover from the first 4 tracks' album artworks (Spotify-style 2×2 grid via the same `smart_playlists::cover::build_composite_cover` compositor). Switches to `0` when the user uploads a manual image via `set_playlist_cover_from_file`; flips back to `1` (and immediately re-runs the auto pipeline) on `clear_playlist_cover`. Smart playlists (`is_smart=1`) are excluded from this hook to keep ownership clean.
- **Full-width music views**: HomeView / LibraryView / PlaylistView / AlbumDetailView / ArtistDetailView / LikedView / RecentView / StatisticsView render full-width inside the center column (no `max-w-*` cap). Track tables are borderless (no `rounded-2xl border bg-white` wrapper) so rows feel "on the page" Spotify-style. Form-style views (Settings, About, Feedback) keep `max-w-4xl` for line-length comfort.

## Conventions

- **Conventional commits**: enforced locally via husky `commit-msg` → `bunx commitlint --edit`. Config in `.commitlintrc.cjs` (header ≤ 100, kebab-case scopes). The `prepare: husky` script auto-installs the hook on `bun install`.
- **PR labels**: `.github/workflows/label-pr.yml` auto-applies `scope:*` (path-based via `actions/labeler`), `type:*` (parsed from PR title prefix), and `size:*` (from diff line count) on every PR open/sync.

## Language

The README is in English. The app ships UI copy in **17 locales** via i18next — `fr` (source of truth), `en`, `es`, `de`, `it`, `nl`, `pt`, `pt-BR`, `ru`, `tr`, `id`, `ja`, `kr` (registered as `ko` + `kr` alias), `zh-CN`, `zh-TW`, `ar`, `hi`. Strings live in `src/i18n/locales/<code>.json`. There is no per-key fallback, so every locale must include every key. `index.ts` sets `document.documentElement.dir` per language so Arabic renders RTL automatically. Non-French locales were bulk-translated from `fr.json` through DeepL with explicit music-player context, then post-processed to keep brand tokens (`WaveFlow`, `Last.fm`, `Deezer`, `ReplayGain`, `LRCLIB`, `BPM`) verbatim and preserve i18next `{{placeholder}}` interpolation.
