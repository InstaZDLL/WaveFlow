<p align="center">
  <img src="logo.svg" width="80" alt="WaveFlow logo" />
</p>

<h1 align="center">WaveFlow</h1>

<p align="center">
  <strong>Local music player for desktop — built with Tauri 2, React 19 & Rust</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-0.1.0-emerald?style=flat-square" alt="Version" />
  <img src="https://img.shields.io/badge/tauri-2.10-blue?style=flat-square&logo=tauri" alt="Tauri 2" />
  <img src="https://img.shields.io/badge/react-19-61dafb?style=flat-square&logo=react" alt="React 19" />
  <img src="https://img.shields.io/badge/rust-stable-orange?style=flat-square&logo=rust" alt="Rust" />
  <img src="https://img.shields.io/badge/license-GPL--3.0-green?style=flat-square" alt="License" />
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey?style=flat-square" alt="Platform" />
</p>

---

WaveFlow is a local music player desktop app with a Spotify-inspired 3-panel UI. It scans your local audio folders, organizes tracks by album/artist/genre, and plays them with a real-time audio engine — no streaming, no cloud, your music stays on your machine.

## Features

- **Audio playback** — symphonia decoder + cpal output, supports MP3, FLAC, WAV, OGG Vorbis, AAC, ALAC (M4A)
- **Real-time engine** — lock-free 3-thread architecture (decoder, ring buffer, cpal callback), zero allocations in the hot path
- **Library scanning** — point to any folder, metadata extraction via lofty, embedded artwork extraction
- **Playlists** — create, edit, delete, add tracks from folders/albums/artists in bulk
- **Likes** — heart any track, dedicated "Liked tracks" view
- **Search** — instant full-text search (FTS5) across titles, artists, albums with prefix matching
- **Queue** — persistent queue with shuffle (Fisher-Yates), repeat (off/all/one), auto-advance
- **Resume** — remembers last track + position across app restarts
- **Audio settings** — volume normalization (-3 dB), mono downmix, crossfade slider (UI ready)
- **Virtual scroll** — handles 6000+ tracks without UI freeze (@tanstack/react-virtual)
- **Dark mode** — animated radial transition via View Transitions API
- **i18n** — French & English, auto-detected, switchable in settings
- **Accessibility** — keyboard navigation, ARIA roles, focus rings, `prefers-reduced-motion`
- **Profiles** — isolated per-profile database (libraries, playlists, settings, play history)

## Tech Stack

| Layer | Technologies |
|-------|-------------|
| **Desktop shell** | Tauri 2.10 |
| **Frontend** | React 19, TypeScript, Vite 8, Tailwind CSS 4, Lucide icons |
| **Backend** | Rust, SQLite (sqlx), FTS5 full-text search |
| **Audio** | symphonia 0.5 (decode), cpal 0.15 (output), rubato 0.15 (resample), rtrb 0.3 (SPSC ring) |
| **Package manager** | Bun |

## Getting Started

```bash
# Install dependencies
bun install

# Run the desktop app in development mode
bun run tauri dev

# Build for production
bun run tauri build
```

## Development Commands

```bash
bun run dev          # Vite dev server only (no Tauri shell)
bun run typecheck    # TypeScript check
bun run lint         # ESLint
bun run lint:fix     # ESLint with auto-fix
bun run format       # Prettier
```

## Project Structure

```
waveflow/
├── src/                              # React frontend
│   ├── components/
│   │   ├── common/                   # Reusable UI (NavItem, Artwork, modals, EmptyState)
│   │   ├── layout/                   # Sidebar, TopBar, AppLayout, QueuePanel
│   │   ├── player/                   # PlayerBar, PlaybackControls, VolumeControl, ProgressBar
│   │   └── views/                    # Home, Library, Playlist, Liked, Recent, Settings, etc.
│   ├── contexts/                     # ThemeContext, PlayerContext, LibraryContext, PlaylistContext
│   ├── hooks/                        # useTheme, usePlayer, useLibrary, usePlaylist, useProfile
│   ├── lib/
│   │   ├── tauri/                    # Typed invoke() wrappers (track, browse, player, playlist)
│   │   ├── playlistVisuals.ts        # Shared color/icon constants for playlists
│   │   └── PlaylistIcon.tsx          # Icon dispatcher component
│   ├── i18n/locales/                 # fr.json, en.json
│   ├── types/                        # ViewId, LibraryTab, NavItemProps, etc.
│   ├── App.tsx                       # Provider tree
│   └── main.tsx                      # Entry point
├── src-tauri/                        # Rust backend
│   ├── src/
│   │   ├── audio/                    # Audio engine (engine, decoder, output, resampler, state, analytics)
│   │   ├── commands/                 # Tauri commands (library, playlist, track, browse, player, scan, profile)
│   │   ├── db/                       # Database open/migrate helpers
│   │   ├── queue.rs                  # Persistent queue operations (fill, advance, shuffle, restore)
│   │   ├── state.rs                  # AppState (profile pool, paths)
│   │   ├── paths.rs                  # Filesystem layout
│   │   ├── error.rs                  # AppError + AppResult
│   │   └── lib.rs                    # Tauri setup, command registration, shutdown hook
│   ├── migrations/
│   │   └── profile/                  # Per-profile SQLite schema (FTS5, triggers, indexes)
│   ├── Cargo.toml
│   └── tauri.conf.json
└── package.json
```

## Audio Architecture

```
┌─ Tauri commands (tokio)     ┌─ Decoder thread (std)        ┌─ cpal callback (real-time)
│  player_play, pause, seek   │  symphonia FormatReader +     │  pop f32 from SPSC ring
│  → crossbeam::Sender ──────►│  Decoder + rubato Resampler   │  × volume × normalization
│                              │  push f32 → rtrb::Producer ──►│  mono downmix (if enabled)
│                              │  emit position/state events   │  → device native format
└──────────────────────────────┴───────────────────────────────┴──────────────────────────
```

**Rules:** the cpal callback never allocates, never locks, never logs. It only touches `rtrb::Consumer` and `Atomic*` fields in `SharedPlayback`.

## i18n

Strings are externalized in `src/i18n/locales/`. To add a language:

1. Create `src/i18n/locales/xx.json` (same structure as `fr.json`)
2. Import it in `src/i18n/index.ts` and add to `SUPPORTED_LANGUAGES`
3. It will appear in the Settings language selector automatically

## License

GPL-3.0 — see [LICENSE](LICENSE)
