# WaveFlow Documentation

User-facing references and per-feature deep dives. The top-level [README](../README.md) keeps the install / quick-start path short — anything substantive about how a feature actually works lives here.

## Features

| Doc | Scope |
|-----|-------|
| [Playback engine](features/playback.md) | Decoder pipeline, crossfade DSP, ReplayGain, output device selection, OS media controls, persistent queue |
| [Library](features/library.md) | Folder scanning, filesystem watcher, on-demand audio analysis, multi-artist split, ratings, A-Z navigator |
| [Playlists](features/playlists.md) | User playlists CRUD, M3U import/export, likes, recently-played |
| [Smart playlists](features/smart-playlists.md) | Daily Mix auto-generation: algorithm, cover compositor, regen flow |
| [Integrations](features/integrations.md) | Deezer / Last.fm / LRCLIB clients, metadata cache, scrobble worker, similar-artists discovery |
| [DLNA / UPnP server](features/dlna.md) | Built-in MediaServer: SSDP discovery, ContentDirectory Browse, Range streaming to LAN amplifiers |
| [UI & UX](features/ui.md) | Layout, panels, tray, statistics, dark mode, i18n, profiles, onboarding, auto-updater |

## Architecture

| Doc | Scope |
|-----|-------|
| [Audio architecture](architecture/audio.md) | 3-thread lock-free pipeline, ring buffer sizing, callback constraints |
| [Database & paths](architecture/storage.md) | `app.db` vs per-profile `data.db`, on-disk layout, migration policy |

## Contributing

[CONTRIBUTING.md](../CONTRIBUTING.md) and [RELEASING.md](../RELEASING.md) cover the contribution and release flows respectively. Anything project-wide that should always be loaded into Claude Code's context lives in [`CLAUDE.md`](../CLAUDE.md) at the repo root.
