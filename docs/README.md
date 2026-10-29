# WaveFlow Documentation

User-facing references and per-feature deep dives. The top-level [README](../README.md) keeps the install / quick-start path short — anything substantive about how a feature actually works lives here.

## Features

| Doc                                            | Scope                                                                                                                                                                                                 |
| ---------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Playback engine](features/playback.md)        | Decoder pipeline, crossfade DSP, ReplayGain, output device selection, OS media controls, persistent queue, A-B loop                                                                                   |
| [Library](features/library.md)                 | Folder scanning + management (add / watch / remove), filesystem watcher, drag-and-drop import, duplicate detection, on-demand audio analysis, multi-artist split, ratings, A-Z navigator              |
| [Playlists](features/playlists.md)             | User playlists CRUD, M3U import/export, likes, recently-played                                                                                                                                        |
| [Smart playlists](features/smart-playlists.md) | Daily Mix auto-generation + user-defined rule editor: algorithm, cover compositor, regen flow                                                                                                         |
| [Integrations](features/integrations.md)       | Deezer / Last.fm / lyrics providers, metadata cache, scrobble worker, similar-artists discovery, in-app lyrics editor                                                                                 |
| [Plugins](features/plugins.md)                 | WASM plugin SDK + sandbox, in-app store (curated catalogue, blake3-verified installs), per-plugin options, official Web Radio + Apple Motion Artwork + Release Radar plugins, motion-artwork pipeline |
| [DLNA / UPnP server](features/dlna.md)         | Built-in MediaServer: SSDP discovery, ContentDirectory Browse, Range streaming to LAN amplifiers                                                                                                      |
| [MPD server](features/mpd.md)                  | Control surface for existing MPD clients (MALP, ncmpcpp, waybar): protocol subset, idle notifications, shared player actions, LAN bind rationale                                                      |
| [Community-DB](features/community.md)          | _Placeholder._ Opt-in shared metadata pool — companion page to [RFC-004](rfcs/RFC-004-community-database.md). Real copy fills in during Phase 2.a.                                                    |
| [UI & UX](features/ui.md)                      | Layout, panels, skins, immersive view, track Canvas, cover slideshow, mini-player widget, tray, statistics, dark mode, i18n, profiles, onboarding, auto-updater                                       |

## Architecture

| Doc                                                    | Scope                                                                                                                                                                                                                                                             |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Cross-cutting invariants](architecture/invariants.md) | The rules that bite when ignored: profile pool leases, settings persistence, migration policy, SQLite writer discipline, hot audio callback, portalled overlays, sync emit contract, plugin boundaries. Long form of the checklist in [`CLAUDE.md`](../CLAUDE.md) |
| [Crate layout](architecture/crates.md)                 | `waveflow-core` vs `waveflow` split rules, feature-flag matrix, re-export shims                                                                                                                                                                                   |
| [Audio architecture](architecture/audio.md)            | 3-thread lock-free pipeline, ring buffer sizing, callback constraints                                                                                                                                                                                             |
| [Database & paths](architecture/storage.md)            | `app.db` vs per-profile `data.db`, on-disk layout, migration policy                                                                                                                                                                                               |

## Design

| Doc                                                        | Scope                                                                                                                                                                                                                                                         |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Fluid Editorial](design/fluid-editorial.md)               | The visual direction since 2.0: principles, colour / type / shape / motion tokens, the wave mark, every component's shape, and how to carry it to the web app, the Android app and the landing site                                                           |
| [Liquid — Aurora Glass](design/liquid-aurora-glass.md)     | The second direction on the same interface: music as light and material — pastel pools tinted by the cover, four glass levels, separation by light, states as light, the mood capsules and the light progress                                                 |
| [Pulse — Neon Deck](design/pulse-neon-deck.md)             | The third material on the same interface: a club control panel — OLED black, the chrome drawn as hairlines, the theme's accent as neon on whatever is live, the tempo deck, the floating pill of a player bar, the spring                                     |
| [Editorial — Broadsheet](design/editorial-broadsheet.md)   | The fourth material on the same interface: a broadsheet — cream paper and ink, rules where the others put tone, the theme's accent as the rubric colour, the masthead over the canvas, the halftone plates, the ruler in picas, the mood band as an engraving |
| [Lounge — Listening Room](design/lounge-listening-room.md) | The fifth material on the same interface: a listening room lit by the record — the cover blurred once per record over the whole window, smoked wine glass that never blurs, the horizon progress, the tuning dial of Mood Radio, and the measured cost        |

## RFCs

Long-form design documents that lock in cross-cutting architectural decisions before implementation. New RFCs live under [`rfcs/`](rfcs/) and are numbered sequentially.

| RFC                                                                                            | Status                | Scope                                                                                                                                                                                                                       |
| ---------------------------------------------------------------------------------------------- | --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [RFC-001 — WaveFlow Server](rfcs/RFC-001-waveflow-server.md)                                   | Accepted              | Server, web, auth, sync, streaming, Phase 1 delivery plan                                                                                                                                                                   |
| [RFC-002 — Plugin SDK](rfcs/RFC-002-plugin-sdk.md)                                             | Draft                 | WASM Component Model plugins for sources / metadata / UI, sideload distribution, desktop + server parity                                                                                                                    |
| [RFC-003 — Sync architecture v2](rfcs/RFC-003-sync-architecture.md)                            | Superseded by RFC-005 | Backfill, HLC ordering, per-entity CRDT conflict resolution. **Not** the server's RFC-003 — see [RFC-005](rfcs/RFC-005-remote-source-and-sync-v2.md#the-rfc-003-naming-trap).                                               |
| [RFC-004 — Community-DB](rfcs/RFC-004-community-database.md)                                   | Draft                 | Opt-in shared metadata pool (lyrics, bios, BPM, etc.), LRCLIB pattern. Schema + endpoints + privacy.                                                                                                                        |
| [RFC-005 — Remote source + sync v2](rfcs/RFC-005-remote-source-and-sync-v2.md)                 | Accepted              | The server catalogue as a separate remote source, `MusicServer` / `SyncProvider` seam, PKCE, journal-based user-data sync.                                                                                                  |
| [RFC-006 — Deduplicating the two catalogues](rfcs/RFC-006-deduplicating-the-two-catalogues.md) | Accepted              | What a content-proven track pair may change above the track. Albums pair on a complete non-empty bijection, artists on unanimity plus at least two confirmed links; nothing pairs before both sides have been reconciled.   |
| [RFC-008 — Phone link](rfcs/RFC-008-phone-link.md)                                             | Draft                 | Pairing a phone over the LAN (one-time code, desktop approval, revocable hashed token), remote control and hand-over both ways, a reviewed three-way sync of local-only user data, consented copies. Build planned for 2.1. |

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) and [RELEASING.md](RELEASING.md) cover the contribution and release flows respectively. [`upstream-blockers.md`](upstream-blockers.md) tracks Tauri-ecosystem issues that affect WaveFlow and the policy for handling them.

[`CLAUDE.md`](../CLAUDE.md) at the repo root is deliberately kept **short** — it's loaded into Claude Code's context on every conversation, so it holds the map plus a one-line form of each invariant and nothing else. New explanatory material belongs in a page under this directory, with `CLAUDE.md` linking to it.
