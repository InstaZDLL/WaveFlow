# WaveFlow Documentation

User-facing references and per-feature deep dives. The top-level [README](../README.md) keeps the install / quick-start path short — anything substantive about how a feature actually works lives here.

## Features

| Doc                                            | Scope                                                                                                                                                                                    |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Playback engine](features/playback.md)        | Decoder pipeline, crossfade DSP, ReplayGain, output device selection, OS media controls, persistent queue, A-B repeat                                                                    |
| [Library](features/library.md)                 | Folder scanning + management (add / watch / remove), filesystem watcher, drag-and-drop import, duplicate detection, on-demand audio analysis, multi-artist split, ratings, A-Z navigator |
| [Playlists](features/playlists.md)             | User playlists CRUD, M3U import/export, likes, recently-played                                                                                                                           |
| [Smart playlists](features/smart-playlists.md) | Daily Mix auto-generation + user-defined rule editor: algorithm, cover compositor, regen flow                                                                                            |
| [Integrations](features/integrations.md)       | Deezer / Last.fm / lyrics providers, metadata cache, scrobble worker, similar-artists discovery, in-app lyrics editor                                                                    |
| [DLNA / UPnP server](features/dlna.md)         | Built-in MediaServer: SSDP discovery, ContentDirectory Browse, Range streaming to LAN amplifiers                                                                                         |
| [Community-DB](features/community.md)          | _Placeholder._ Opt-in shared metadata pool — companion page to [RFC-004](rfcs/RFC-004-community-database.md). Real copy fills in during Phase 2.a.                                       |
| [UI & UX](features/ui.md)                      | Layout, panels, skins, mini-player widget, tray, statistics, dark mode, i18n, profiles, onboarding, auto-updater                                                                         |

## Architecture

| Doc                                         | Scope                                                                 |
| ------------------------------------------- | --------------------------------------------------------------------- |
| [Audio architecture](architecture/audio.md) | 3-thread lock-free pipeline, ring buffer sizing, callback constraints |
| [Database & paths](architecture/storage.md) | `app.db` vs per-profile `data.db`, on-disk layout, migration policy   |

## RFCs

Long-form design documents that lock in cross-cutting architectural decisions before implementation. New RFCs live under [`rfcs/`](rfcs/) and are numbered sequentially.

| RFC                                                                 | Status   | Scope                                                                                                    |
| ------------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------- |
| [RFC-001 — WaveFlow Server](rfcs/RFC-001-waveflow-server.md)        | Accepted | Server, web, auth, sync, streaming, Phase 1 delivery plan                                                |
| [RFC-002 — Plugin SDK](rfcs/RFC-002-plugin-sdk.md)                  | Draft    | WASM Component Model plugins for sources / metadata / UI, sideload distribution, desktop + server parity |
| [RFC-003 — Sync architecture v2](rfcs/RFC-003-sync-architecture.md) | Draft    | Backfill, HLC ordering, per-entity CRDT conflict resolution, status UI. Supersedes RFC-001 §1.f.         |
| [RFC-004 — Community-DB](rfcs/RFC-004-community-database.md)        | Draft    | Opt-in shared metadata pool (lyrics, bios, BPM, etc.), LRCLIB pattern. Schema + endpoints + privacy.     |

## Contributing

[CONTRIBUTING.md](../CONTRIBUTING.md) and [RELEASING.md](../RELEASING.md) cover the contribution and release flows respectively. [`upstream-blockers.md`](upstream-blockers.md) tracks Tauri-ecosystem issues that affect WaveFlow and the policy for handling them. Anything project-wide that should always be loaded into Claude Code's context lives in [`CLAUDE.md`](../CLAUDE.md) at the repo root.
