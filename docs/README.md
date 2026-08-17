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
| [Plugins](features/plugins.md)                 | WASM plugin SDK + sandbox, in-app store (curated catalogue, blake3-verified installs), per-plugin options, official Web Radio + Apple Motion Artwork plugins, motion-artwork pipeline    |
| [DLNA / UPnP server](features/dlna.md)         | Built-in MediaServer: SSDP discovery, ContentDirectory Browse, Range streaming to LAN amplifiers                                                                                         |
| [MPD server](features/mpd.md)                  | Control surface for existing MPD clients (MALP, ncmpcpp, waybar): protocol subset, idle notifications, shared player actions, LAN bind rationale                                         |
| [Community-DB](features/community.md)          | _Placeholder._ Opt-in shared metadata pool — companion page to [RFC-004](rfcs/RFC-004-community-database.md). Real copy fills in during Phase 2.a.                                       |
| [UI & UX](features/ui.md)                      | Layout, panels, skins, immersive view, track Canvas, cover slideshow, mini-player widget, tray, statistics, dark mode, i18n, profiles, onboarding, auto-updater                          |

## Architecture

| Doc                                                    | Scope                                                                                                                                                                                                                                                             |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Cross-cutting invariants](architecture/invariants.md) | The rules that bite when ignored: profile pool leases, settings persistence, migration policy, SQLite writer discipline, hot audio callback, portalled overlays, sync emit contract, plugin boundaries. Long form of the checklist in [`CLAUDE.md`](../CLAUDE.md) |
| [Crate layout](architecture/crates.md)                 | `waveflow-core` vs `waveflow` split rules, feature-flag matrix, re-export shims                                                                                                                                                                                   |
| [Audio architecture](architecture/audio.md)            | 3-thread lock-free pipeline, ring buffer sizing, callback constraints                                                                                                                                                                                             |
| [Database & paths](architecture/storage.md)            | `app.db` vs per-profile `data.db`, on-disk layout, migration policy                                                                                                                                                                                               |

## RFCs

Long-form design documents that lock in cross-cutting architectural decisions before implementation. New RFCs live under [`rfcs/`](rfcs/) and are numbered sequentially.

| RFC                                                                 | Status   | Scope                                                                                                    |
| ------------------------------------------------------------------- | -------- | -------------------------------------------------------------------------------------------------------- |
| [RFC-001 — WaveFlow Server](rfcs/RFC-001-waveflow-server.md)        | Accepted | Server, web, auth, sync, streaming, Phase 1 delivery plan                                                |
| [RFC-002 — Plugin SDK](rfcs/RFC-002-plugin-sdk.md)                  | Draft    | WASM Component Model plugins for sources / metadata / UI, sideload distribution, desktop + server parity |
| [RFC-003 — Sync architecture v2](rfcs/RFC-003-sync-architecture.md) | Superseded by RFC-005 | Backfill, HLC ordering, per-entity CRDT conflict resolution. **Not** the server's RFC-003 — see [RFC-005](rfcs/RFC-005-remote-source-and-sync-v2.md#the-rfc-003-naming-trap). |
| [RFC-004 — Community-DB](rfcs/RFC-004-community-database.md)        | Draft    | Opt-in shared metadata pool (lyrics, bios, BPM, etc.), LRCLIB pattern. Schema + endpoints + privacy.     |
| [RFC-005 — Remote source + sync v2](rfcs/RFC-005-remote-source-and-sync-v2.md) | Accepted | The server catalogue as a separate remote source, `MusicServer` / `SyncProvider` seam, PKCE, journal-based user-data sync. |

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) and [RELEASING.md](RELEASING.md) cover the contribution and release flows respectively. [`upstream-blockers.md`](upstream-blockers.md) tracks Tauri-ecosystem issues that affect WaveFlow and the policy for handling them.

[`CLAUDE.md`](../CLAUDE.md) at the repo root is deliberately kept **short** — it's loaded into Claude Code's context on every conversation, so it holds the map plus a one-line form of each invariant and nothing else. New explanatory material belongs in a page under this directory, with `CLAUDE.md` linking to it.
