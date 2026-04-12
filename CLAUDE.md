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
```

## Architecture

### Frontend (`src/`)

React 19 + TypeScript. Entry point: `src/main.tsx` → `src/App.tsx`. Vite dev server on port 1420.

- **Contexts**: `ThemeContext`, `PlayerContext`, `LibraryContext`, `PlaylistContext`, `ProfileContext` — mounted in `App.tsx` as a provider tree.
- **Hooks**: `useTheme`, `usePlayer`, `useLibrary`, `usePlaylist`, `useProfile` — each wraps its context.
- **Tauri wrappers** (`src/lib/tauri/`): typed `invoke()` wrappers for every backend command (`track.ts`, `browse.ts`, `player.ts`, `playlist.ts`, `library.ts`, `detail.ts`, `integration.ts`). All commands are camelCase on the frontend, snake_case on the backend.
- **Views**: `HomeView`, `LibraryView`, `PlaylistView`, `AlbumDetailView`, `ArtistDetailView`, `LikedView`, `RecentView`, `SettingsView`, etc.
- **Common components**: `ArtistLink` (per-name clickable multi-artist renderer), `Artwork` (file-scoped asset protocol resolver), playlist visuals.
- **Layout**: Apple Music-style sidebar (Ma musique sub-navs + Playlists section), TopBar with search, PlayerBar at bottom, two right-edge panels (`QueuePanel` + `NowPlayingPanel`) mutually exclusive via `PlayerContext`.

### Backend (`src-tauri/`)

Rust/Tauri 2. Entry point: `src-tauri/src/main.rs` → `lib.rs`.

- **Commands** (`src-tauri/src/commands/`): organized by domain — `library.rs`, `playlist.rs`, `track.rs`, `browse.rs`, `player.rs`, `scan.rs`, `profile.rs`, `deezer.rs` (metadata enrichment), `integration.rs` (Last.fm API key storage), `app_info.rs`. All registered in `lib.rs` via `generate_handler![]`.
- **External API clients** (crate-root modules): `deezer.rs` (public Deezer API, no auth) and `lastfm.rs` (Last.fm `artist.getInfo`, requires user-provided API key). Both use `reqwest` with `rustls-tls`.
- **Audio engine** (`src-tauri/src/audio/`): 3-thread lock-free architecture:
  - `engine.rs` — `AudioCmd` enum, `AudioEngine` handle
  - `decoder.rs` — symphonia decode loop, rubato resampling
  - `output.rs` — cpal callback on dedicated thread, SPSC ring buffer (rtrb)
  - `state.rs` — `SharedPlayback` with atomics (no locks in hot path)
  - `analytics.rs` — tokio task for play_event writes + auto-advance
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
- **Metadata cache**: Deezer (pictures, fans) and Last.fm (bios) results are cached in the `deezer_artist` / `deezer_album` profile tables with a 30-day `expires_at` TTL. Cache-first flow in `enrich_artist_deezer` — zero network if the row is fresh. Failures are non-fatal (empty enrichment returned, UI shows local data). The Last.fm API key is read from `app_setting` via `integration::read_lastfm_api_key` and is optional: without it, bios are skipped silently.

## Language

The README is in English. The UI copy is bilingual (French/English) via i18next — strings in `src/i18n/locales/{fr,en}.json`.
