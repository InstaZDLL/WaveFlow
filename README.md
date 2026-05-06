<p align="center">
  <img src="assets/logo.svg" width="80" alt="WaveFlow logo" />
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

### Playback engine

- **Audio playback** — symphonia decoder + cpal output, supports MP3, FLAC, WAV, OGG Vorbis, AAC, ALAC (M4A)
- **Real-time engine** — lock-free 3-thread architecture (decoder, ring buffer, cpal callback), zero allocations in the hot path
- **Crossfade DSP** — real dual-decoder mix with equal-power gains (cos/sin) over the user-set window
- **Audio settings** — volume normalization (-3 dB), mono downmix, configurable crossfade, optional per-track ReplayGain application (uses the value from `track_analysis`, applied per-stream so the crossfade mix gives each side its own gain)
- **Output device picker** — pick any cpal-enumerated device (ALSA hints on Linux); choice persisted per profile and prefetched on launch so playback stays on the user's preferred sink across restarts
- **OS media controls** — SMTC on Windows, MPRIS on Linux, MediaRemote on macOS (via `souvlaki`); Now Playing artwork served to SMTC over a tiny localhost HTTP shim
- **Resume** — remembers last track + position across app restarts
- **Queue** — persistent queue with shuffle (Fisher-Yates), repeat (off/all/one), auto-advance, drag-and-drop reorder

### Library

- **Scanning** — point to any folder, metadata extraction via lofty, embedded artwork extraction
- **Watch folders** — `notify`-driven filesystem watcher, debounced rescans so dropped-in files appear automatically; deleted files are flagged unavailable rather than purged so play history survives
- **Track analysis** — on-demand or auto-after-scan: peak, loudness (dB), ReplayGain, BPM via autocorrelation; tagged musical key (`TKEY` / `INITIALKEY`) read at scan time
- **Audio quality surfacing** — sample rate / bitrate / size / codec / bit depth strip under the player; Hi-Res badges (≥ 24-bit, ≥ 44.1 kHz) on covers and rows
- **Track Properties dialog** — foobar2000-style modal with metadata, audio specs, analysis results, file path, and Show in Explorer
- **POPM ratings** — 5-star ratings (with half-steps) extracted from tags + editable inline
- **Multi-select** — ctrl/shift selection across views with floating action bar (Play / Add to queue / Add to playlist / Remove)
- **Multi-artist** — automatic split of `"Artist A, Artist B"` into individual, independently-linkable artists
- **Album & artist detail pages** — clickable cards open dedicated views with tracklist, discography, biography, and stats
- **Cover picker** — manual Deezer search, local file upload (magic-byte validation), batch fetch for albums missing artwork
- **A-Z navigator** — letter rail on the artists tab, NFD-normalized for diacritics
- **Lightbox** — double-click any cover or artist photo to view full-size

### Playlists & navigation

- **Playlists** — create, edit, delete; add tracks from folders/albums/artists in bulk; drag-and-drop reorder (virtualized for large playlists)
- **Import / export M3U** — write any playlist out as a `.m3u8` (UTF-8, with `#EXTINF` metadata) and re-import `.m3u` / `.m3u8` files from foobar2000, VLC, Rekordbox or hand-written sets. Imports match against the active library by canonical path with a basename fallback for moved files; unmatched entries are surfaced (truncated to 20) for the user to investigate
- **Likes** — heart any track, dedicated "Liked tracks" view
- **Recent** — automatic 50-track recency list driven by `play_event`
- **Search** — instant full-text search (FTS5 contentless) across titles, artists, albums with prefix matching
- **Right-click context menu** — Spotify-style: Play next, Add to queue, Add to playlist (submenu), Like, Go to album, Go to artist (submenu when multi-artist), Properties, Show in explorer

### Integrations

- **Metadata enrichment** — Deezer public API (artist images, album covers, labels) + Last.fm (artist biographies) cached 30 days locally; artwork downloaded into a hash-addressed on-disk cache so it renders offline on re-visits
- **Last.fm scrobbling** — signed `auth.getMobileSession` login, retry queue with exponential backoff, live `track.updateNowPlaying`, automatic re-auth prompt on session expiry
- **Synchronized lyrics** — LRCLIB lookup with embedded-tag fallback and `.lrc` file import

### UI & UX

- **System tray** — quick playback controls + close-to-tray
- **Now Playing / Lyrics panels** — Spotify-style right-edge panels (large artwork, clickable artists, artist biography, synchronized lyrics)
- **Statistics view** — KPIs, listening-by-day / listening-by-hour charts, top tracks / artists / albums
- **Thumbnails** — SIMD-accelerated 1x / 2x covers via `fast_image_resize`
- **Virtual scroll** — handles 6000+ tracks without UI freeze (`@tanstack/react-virtual`)
- **Single-click play** — optional toggle, sort memory persisted per context
- **Dark mode** — animated radial transition via View Transitions API
- **i18n** — 17 languages (FR, EN, ES, DE, IT, NL, PT, PT-BR, RU, TR, ID, JA, KO, ZH-CN, ZH-TW, AR, HI); auto-detected, switchable in settings, RTL-aware
- **Accessibility** — keyboard navigation, ARIA roles, focus rings, `prefers-reduced-motion`
- **Profiles** — isolated per-profile database (libraries, playlists, settings, play history); shared metadata cache across profiles
- **First-run onboarding** — modal prompts new profiles to point at a music folder; the "configure later" choice latches per profile so the prompt never reappears
- **Auto-updater** — Tauri updater plugin with signed update flow; the update banner offers "Install now" without forcing a relaunch interruption

## Tech Stack

| Layer | Technologies |
|-------|-------------|
| **Desktop shell** | Tauri 2.10 (tray icon, opener, dialog, updater plugins) |
| **OS media controls** | souvlaki 0.8 (SMTC / MPRIS / MediaRemote bridge) |
| **Frontend** | React 19, TypeScript, Vite 8, Tailwind CSS 4, Lucide icons, `@dnd-kit` (drag-and-drop), `@tanstack/react-virtual` (virtualization) |
| **Backend** | Rust, SQLite (sqlx), FTS5 contentless full-text search |
| **Audio** | symphonia 0.5 (decode), cpal 0.15 (output), rubato 0.15 (resample), rtrb 0.3 (SPSC ring) |
| **Metadata extraction** | lofty 0.22 (tags, embedded art, POPM, INITIALKEY) |
| **Imaging** | image 0.25 + fast_image_resize 6 (SIMD thumbnails) |
| **Filesystem watcher** | notify 8 (debounced rescans of watched folders) |
| **External APIs** | Deezer public API (no auth) + Last.fm (read + signed methods via md-5 + reqwest 0.12 with rustls) + LRCLIB (synchronized lyrics) |
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

# Rust backend
cargo check --manifest-path src-tauri/Cargo.toml --all-targets
cargo test  --manifest-path src-tauri/Cargo.toml
```

## Project Structure

```
waveflow/
├── src/                              # React frontend
│   ├── components/
│   │   ├── common/                   # Reusable UI (Artwork, ArtistLink, ContextMenu, TrackContextMenu, TrackPropertiesModal, HiResBadge, Lightbox, CoverPickerModal, StarRating, SelectionActionBar, modals, EmptyState…)
│   │   ├── layout/                   # Sidebar, TopBar, AppLayout, QueuePanel, NowPlayingPanel, LyricsPanel, DeviceMenu
│   │   ├── player/                   # PlayerBar, PlaybackControls, VolumeControl, ProgressBar, AudioQualityFooter
│   │   └── views/                    # Home, Library, Playlist, AlbumDetail, ArtistDetail, Liked, Recent, Settings, Statistics…
│   ├── contexts/                     # ThemeContext, PlayerContext, LibraryContext, PlaylistContext, ProfileContext, PageScrollContext
│   ├── hooks/                        # useTheme, usePlayer, useLibrary, usePlaylist, useProfile, useTrackContextMenu, useMultiSelect, useSortMemory
│   ├── lib/
│   │   ├── tauri/                    # Typed invoke() wrappers (track, browse, player, playlist, detail, integration, analysis, lyrics, stats, profile, dialog, deezer, library, artwork)
│   │   ├── hiRes.ts                  # `isHiRes` helper (≥ 24-bit, ≥ 44.1 kHz threshold)
│   │   ├── imageCache.ts             # In-memory LRU for resolved artwork URLs
│   │   ├── playlistVisuals.ts        # Shared color/icon constants for playlists
│   │   └── PlaylistIcon.tsx          # Icon dispatcher component
│   ├── i18n/locales/                 # fr/en/es/de/it/nl/pt/pt-BR/ru/tr/id/ja/kr/zh-CN/zh-TW/ar/hi.json
│   ├── types/                        # ViewId, LibraryTab, NavItemProps, etc.
│   ├── App.tsx                       # Provider tree
│   └── main.tsx                      # Entry point
├── src-tauri/                        # Rust backend
│   ├── src/
│   │   ├── audio/                    # Audio engine (engine, decoder, output, resampler, state, analytics, crossfade)
│   │   ├── commands/                 # Tauri commands (library, playlist, track, browse, player, scan, profile, deezer, integration, lyrics, stats, analysis, maintenance, app_info)
│   │   ├── db/                       # Database open/migrate helpers (app.db + per-profile data.db)
│   │   ├── analysis.rs               # Per-track audio analysis (peak, loudness dB, ReplayGain, BPM)
│   │   ├── deezer.rs                 # Deezer public API client (search/get artist & album)
│   │   ├── lastfm.rs                 # Last.fm API client (artist.getInfo + signed mobile-session / scrobble / now-playing)
│   │   ├── lrclib.rs                 # LRCLIB API client (synchronized lyrics)
│   │   ├── media_controls.rs         # OS media-key bridge (SMTC / MPRIS / MediaRemote via souvlaki)
│   │   ├── metadata_artwork.rs       # Shared on-disk cache for remote artwork (blake3-hashed)
│   │   ├── queue.rs                  # Persistent queue operations (fill, advance, shuffle, reorder, restore)
│   │   ├── scrobbler.rs              # Last.fm scrobble worker (queue drain, retry/backoff, re-auth prompt)
│   │   ├── thumbnails.rs             # SIMD-accelerated 1x/2x cover thumbnails
│   │   ├── watcher.rs                # Filesystem watcher manager (per-folder notify watchers, debounced rescans)
│   │   ├── state.rs                  # AppState (profile pool, paths, global app_db)
│   │   ├── paths.rs                  # Filesystem layout
│   │   ├── error.rs                  # AppError + AppResult
│   │   └── lib.rs                    # Tauri setup, command registration, system tray, shutdown hook
│   ├── migrations/
│   │   ├── app/                      # Global app.db schema (profile list, app_setting, shared metadata cache: metadata_artist, metadata_album, lyrics)
│   │   └── profile/                  # Per-profile SQLite schema (FTS5 contentless, triggers, indexes, scrobble_queue, track_analysis, audio quality columns)
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

Currently shipping 17 languages — French, English, Spanish, German, Italian, Dutch, Portuguese (PT and BR), Russian, Turkish, Indonesian, Japanese, Korean, Chinese (Simplified and Traditional), Arabic, and Hindi. Auto-detected at first launch from the OS locale, switchable from Settings. Arabic ships with RTL layout (`dir="rtl"`).

Strings are externalized in `src/i18n/locales/`. The French file is the source of truth — non-French locales were generated from `fr.json` via DeepL with music-player context, then audited for placeholder integrity and brand-name preservation. To add a language:

1. Create `src/i18n/locales/xx.json` (same structure as `fr.json` — keep all keys translated, the loader doesn't fall back per-key)
2. Import it in `src/i18n/index.ts` and add to `SUPPORTED_LANGUAGES`
3. It will appear in the Settings language selector automatically

## License

GPL-3.0 — see [LICENSE](LICENSE)
