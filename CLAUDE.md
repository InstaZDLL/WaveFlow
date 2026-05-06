# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

WaveFlow is a local music player desktop app built with **Tauri 2 + React 19 + TypeScript + Vite**. It uses a Spotify/Apple Music-inspired UI for browsing and playing local audio files. The project uses **bun** as the package manager.

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

- **Commands** (`src-tauri/src/commands/`): organized by domain — `library.rs`, `playlist.rs` (CRUD + M3U import/export), `track.rs`, `browse.rs`, `player.rs` (playback + output device list/select), `scan.rs`, `profile.rs`, `analysis.rs` (peak/loudness/ReplayGain/BPM), `deezer.rs` (metadata enrichment), `lyrics.rs` (LRCLIB + embedded fallback + .lrc import), `stats.rs` (listening analytics from `play_event`), `integration.rs` (Last.fm API key storage), `maintenance.rs`, `app_info.rs`. All registered in `lib.rs` via `generate_handler![]`.
- **External API clients** (crate-root modules): `deezer.rs` (public Deezer API, no auth) and `lastfm.rs` (Last.fm `artist.getInfo`, requires user-provided API key). Both use `reqwest` with `rustls-tls`.
- **OS media controls** (`src-tauri/src/media_controls.rs`): souvlaki bridge wired to SMTC / MPRIS / MediaRemote. Initialized after the main window exists (needs HWND on Windows). Now Playing artwork is served to SMTC over a tiny localhost HTTP shim because Windows expects a URL, not a file path.
- **Audio engine** (`src-tauri/src/audio/`): 3-thread lock-free architecture:
  - `engine.rs` — `AudioCmd` enum, `AudioEngine` handle
  - `decoder.rs` — symphonia decode loop, rubato resampling
  - `output.rs` — cpal callback on dedicated thread, SPSC ring buffer (rtrb), output-device enumeration (ALSA hints on Linux, `cpal` elsewhere)
  - `state.rs` — `SharedPlayback` with atomics (no locks in hot path)
  - `analytics.rs` — tokio task for play_event writes + auto-advance
  - `crossfade.rs` — dual-decoder mix with equal-power gain curves over the user-set window
- **Queue** (`src-tauri/src/queue.rs`): persistent queue with fill, advance, shuffle (Fisher-Yates), restore.
- **Database**: per-profile SQLite via sqlx + a global `app.db` for profile list and app-wide settings (`app_setting` table — including the Last.fm API key). Single migration in `migrations/profile/`. FTS5 contentless for search with auto-sync triggers using the `'delete'` command.

### Key Patterns

- **Tauri commands**: `#[tauri::command]` in `commands/*.rs`, registered in `lib.rs` `generate_handler![]`, called from React with `invoke("command_name", { args })`.
- **Profile-scoped pool**: `state.require_profile_pool().await?` — every command that touches user data goes through this.
- **Persistence**: settings stored in `profile_setting` table (key-value with typed values). Pattern: `INSERT ... ON CONFLICT DO UPDATE`.
- **Events**: backend emits Tauri events (`player:state`, `player:position`, `player:track-changed`, `player:queue-changed`, `player:error`). Frontend listens via `listen()` from `@tauri-apps/api/event`.
- **Audio callback constraints**: the cpal callback MUST NOT allocate, lock, or log. Only `rtrb::Consumer` + `Atomic*` reads.
- **Virtual scroll**: TrackTable uses `@tanstack/react-virtual` for 6000+ track performance.
- **Multi-artist**: the scanner splits `"A, B"` on `", " / "; "` into individual `artist` rows linked via the `track_artist` many-to-many table. Queries rebuild the display string via `GROUP_CONCAT` over `track_artist` ordered by `position`. `ArtistLink` accepts parallel `artist_name` + `artist_ids` strings so every contributor is individually clickable.
- **Metadata cache**: Deezer (pictures, fans) and Last.fm (bios) results are cached in the `deezer_artist` / `deezer_album` tables stored in `app.db` (shared across profiles) with a 30-day `expires_at` TTL. Cache-first flow in `enrich_artist_deezer` — zero network if the row is fresh. Failures are non-fatal (empty enrichment returned, UI shows local data). The Last.fm API key is read from `app_setting` via `integration::read_lastfm_api_key` and is optional: without it, bios are skipped silently.
- **Remote artwork on disk**: Deezer pictures/covers are downloaded into the shared `<root>/metadata_artwork/<blake3>.jpg` cache by `metadata_artwork::download_and_cache`. The blake3 hash is persisted in `deezer_artist.picture_hash` / `deezer_album.cover_hash` (Deezer always serves JPEG, hence the hardcoded extension). Enrichment commands and `list_artists` / `get_artist_detail` / `stats_top_artists` return both `picture_url` (remote fallback) and `picture_path` (absolute local path); the frontend helper `lib/tauri/artwork.ts::resolveRemoteImage` prefers the local file via `convertFileSrc` so artist imagery renders offline. The `metadata_artwork/**` scope must stay listed in `tauri.conf.json` `assetProtocol`.
- **Output device persistence**: the chosen device's `name` is stored in `profile_setting['audio.output_device']`. `lib.rs` reads it during `setup` and forwards it to the audio engine, so playback resumes on the user's preferred sink without waiting for the frontend to settle.
- **First-run onboarding**: `AppLayout` latches a single decision per profile via `profile_setting['onboarding.dismissed']`. The modal only appears when the profile is fully resolved AND the library is empty AND the flag isn't set — preventing flashes during boot transitions.
- **Page-level scrolling**: virtualized tables in `PlaylistView` / `LibraryView` consume `usePageScroll()` for the scroll element instead of nesting their own `overflow-y-auto`. They compute `scrollMargin` from the parent's offset within the page scroller so `useVirtualizer` knows where their content begins. Drives a single Spotify-style scrollbar.

## Conventions

- **Conventional commits**: enforced locally via husky `commit-msg` → `bunx commitlint --edit`. Config in `.commitlintrc.cjs` (header ≤ 100, kebab-case scopes). The `prepare: husky` script auto-installs the hook on `bun install`.
- **PR labels**: `.github/workflows/label-pr.yml` auto-applies `scope:*` (path-based via `actions/labeler`), `type:*` (parsed from PR title prefix), and `size:*` (from diff line count) on every PR open/sync.

## Language

The README is in English. The app ships UI copy in **17 locales** via i18next — `fr` (source of truth), `en`, `es`, `de`, `it`, `nl`, `pt`, `pt-BR`, `ru`, `tr`, `id`, `ja`, `kr` (registered as `ko` + `kr` alias), `zh-CN`, `zh-TW`, `ar`, `hi`. Strings live in `src/i18n/locales/<code>.json`. There is no per-key fallback, so every locale must include every key. `index.ts` sets `document.documentElement.dir` per language so Arabic renders RTL automatically. Non-French locales were bulk-translated from `fr.json` through DeepL with explicit music-player context, then post-processed to keep brand tokens (`WaveFlow`, `Last.fm`, `Deezer`, `ReplayGain`, `LRCLIB`, `BPM`) verbatim and preserve i18next `{{placeholder}}` interpolation.
