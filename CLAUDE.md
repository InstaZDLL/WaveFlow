# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

WaveFlow is a local music player desktop app built with **Tauri 2 + React 19 + TypeScript + Vite** and a **bun** toolchain. Spotify / Apple Music-inspired UI on top of a Rust audio engine.

This file is an **index, not a manual**. It carries the map plus the one-line form of each invariant. The reasoning, algorithms, schema and flow diagrams live under [`docs/`](docs/README.md) — that's the source of truth, and new detail belongs there, not here.

| Need                                    | Read                                                                 |
| --------------------------------------- | -------------------------------------------------------------------- |
| Why a rule exists / how not to break it | [`docs/architecture/invariants.md`](docs/architecture/invariants.md) |
| How a feature actually works            | [`docs/features/`](docs/README.md#features)                          |
| Crate split, audio topology, DB layout  | [`docs/architecture/`](docs/README.md#architecture)                  |
| Design decisions not yet built          | [`docs/rfcs/`](docs/README.md#rfcs)                                  |
| Release / packaging procedure           | [`docs/RELEASING.md`](docs/RELEASING.md)                             |

## Development Commands

```bash
bun install                  # dependencies

bun run tauri dev            # full desktop app (Vite + Rust)
bun run tauri build          # production bundle

bun run dev                  # Vite dev server only (no Tauri shell), port 1420
bun run typecheck            # tsc --noEmit
bun run lint                 # eslint
bun run build                # tsc + Vite prod build

cargo check --manifest-path src-tauri/Cargo.toml --workspace --all-targets
cargo test  --manifest-path src-tauri/Cargo.toml --workspace
```

The PR checklist is the `typecheck` / `lint` / `cargo check` triple.

## Architecture

### Frontend (`src/`)

React 19 + TypeScript. Entry: `src/main.tsx` → `src/App.tsx`.

- **Contexts** (provider tree in `App.tsx`): `ThemeContext`, `PlayerContext`, `LibraryContext`, `PlaylistContext`, `ProfileContext`. `PageScrollContext` mounts lower (in `AppLayout`) and exposes the main scrollable area to virtualized tables — one page-driven scrollbar.
- **Hooks** wrap each context: `useTheme`, `usePlayer`, `useLibrary`, `usePlaylist`, `useProfile`, `usePageScroll`.
- **Tauri wrappers** (`src/lib/tauri/`): one typed `invoke()` per backend command.
- **Views**: `HomeView`, `LibraryView`, `PlaylistView`, `AlbumDetailView`, `ArtistDetailView`, `LikedView`, `HistoryView`, `StatisticsView`, `WrappedView`, `SettingsView`, …
- **Layout**: Apple-Music-style sidebar, TopBar with search, PlayerBar at the bottom, right-edge panels (`NowPlayingPanel` / `QueuePanel` / `LyricsPanel`) mutex'd via `PlayerContext`. A second `WebviewWindow` (label `mini`, `?mini=1`) ships the always-on-top mini-player — [`docs/features/ui.md`](docs/features/ui.md#mini-player).

### Backend (`src-tauri/`) — Cargo workspace, two members

- **`crates/core/` (`waveflow-core`)** — portable business logic, reusable from `waveflow-server`. Domain DTOs, repository traits + SQLite **and** Postgres impls (Cargo features: desktop = `sqlite`, server = `postgres`), scanner helpers + upserts, smart-playlist engine, audio analysis, DSD→PCM, HTTP clients (Deezer / Last.fm / LRCLIB / TheAudioDB), artwork pipeline, the wasmtime plugin host. **Zero Tauri / `cpal`.** Split rules + feature matrix: [`docs/architecture/crates.md`](docs/architecture/crates.md).
- **`crates/app/` (`waveflow`)** — the Tauri 2 app. Entry `crates/app/src/main.rs` → `lib.rs`. `#[tauri::command]` handlers (thin wrappers over core's repositories), the real-time `cpal` + `rtrb` audio engine, DLNA / MPD / OS media controls / Discord RPC, the fs watcher, the tray, the profile pool wiring.

Inside `crates/app/src/`:

- **`commands/`** — one module per domain (`library`, `playlist`, `smart_playlists`, `track`, `browse`, `player`, `scan`, `edit`, `profile`, `analysis`, `deezer`, `similar`, `lyrics`, `stats`, `wrapped`, `maintenance`, `radio`, `duplicates`, `preferences`, `plugins`, `canvas`, …), all registered in `lib.rs::generate_handler![]`. CRUD delegates to `waveflow_core::repository::sqlite::*`; IPC + state + filesystem + emit glue stays in the command.
- **`audio/`** — 3-thread lock-free engine: `decoder.rs` (symphonia + rubato), `output.rs` (cpal callback on its own thread, SPSC `rtrb` ring), `state.rs` (`SharedPlayback` atomics), `analytics.rs`, `crossfade.rs`, `eq.rs`, `spectrum.rs`, `wasapi_exclusive.rs`. Topology: [`docs/architecture/audio.md`](docs/architecture/audio.md).
- **`dlna/`** (axum + SSDP, opt-in) · **`mpd/`** (TCP MPD protocol, opt-in) · **`media_controls.rs`** (souvlaki → SMTC / MPRIS / MediaRemote) · **`discord_presence.rs`** · **`queue.rs`** · **`player_actions.rs`** (shared control sequence) · **`remote/`** (remote source + sync v2, feature `sync_v2`, now in the default feature set; `mod sync` is now a permanent no-op stub — v1 was removed in the RFC-005 cutover) · **`backup.rs`** · **`db/`** (pool wiring + `migration_heal`).
- **Scanner** — the orchestrator `scan_folder_inner` stays app-side (it emits `scan:progress`); every pure helper lives in `waveflow_core::scanner::{extract, upserts}`.
- **Database** — per-profile SQLite via sqlx + a global `app.db` for the profile list and app-wide settings. Migrations at `src-tauri/migrations/{app,profile}/`, compiled in via `sqlx::migrate!`. Layout: [`docs/architecture/storage.md`](docs/architecture/storage.md).

## Cross-cutting rules (always apply)

One line each. **The reasoning, the failure mode and the exceptions are in [`docs/architecture/invariants.md`](docs/architecture/invariants.md) — read the matching section before touching one of these.**

- **Tauri commands** — `commands/*.rs` + `generate_handler![]`; frontend camelCase, backend snake_case.
- **Profile-scoped pool** — `state.require_profile_pool().await?` for anything touching user data; it's a _leased_ `ProfilePool`, so query with `&*pool`, keep the handle bound, and never re-resolve it mid-batch. [→](docs/architecture/invariants.md#profile-scoped-pool)
- **Settings persistence** — `profile_setting` / `app_setting` via `INSERT … ON CONFLICT DO UPDATE`; a React preference hook builds on [`useProfileSetting`](src/hooks/useProfileSetting.ts), never hand-rolled. [→](docs/architecture/invariants.md#persistence-of-settings)
- **Events** — backend emits `player:*`, `track:updated`, `library:rescanned`, `scan:progress`, `lyrics:updated`, …; frontend uses `listen()`. [→](docs/architecture/invariants.md#events)
- **Audio callback is hot** — no allocation, no locks, no logging; all DSP happens on the decoder thread, whose last stage is a `[-1.0, 1.0]` clamp. [→](docs/architecture/invariants.md#the-audio-callback-is-hot)
- **Never `DROP TABLE` a parent table in a migration** — `foreign_keys = ON` turns it into a cascading delete. Use `ALTER TABLE … ADD COLUMN`. [→](docs/architecture/invariants.md#never-drop-table-a-parent-table-in-a-migration)
- **Migrations are immutable once merged** — sqlx checksums them; always add a new dated `YYYYMMDDhhmmss_<slug>.sql`. [→](docs/architecture/invariants.md#migrations-are-immutable-once-merged)
- **Single writer to SQLite** — batch in transactions, pass `&mut SqliteConnection` to upsert helpers, and any background writer parks behind the scan + retries on `SQLITE_BUSY`. [→](docs/architecture/invariants.md#single-writer-to-sqlite)
- **Multi-artist split is `"; "` only** — never `", "`; queries rebuild credits by joining `track_artist` with `GROUP_CONCAT` ordered by `position`. [→](docs/architecture/invariants.md#multi-artist-queries)
- **Album grouping = `(canonical_title, album_artist_id)`** — `is_compilation` is sticky and renames reuse the old album's flags. [→](docs/architecture/invariants.md#album-grouping--canonical_title-album_artist_id)
- **File-write safety on Windows** — pause playback before rewriting the current track's file, then re-hash blake3 into `track.file_hash`. [→](docs/architecture/invariants.md#file-write-safety-on-windows)
- **Virtual scroll everywhere** — `@tanstack/react-virtual` + `usePageScroll()`, never a nested `overflow-y-auto`. [→](docs/architecture/invariants.md#virtual-scroll-everywhere)
- **Modal accessibility** — every modal calls [`useModalA11y`](src/hooks/useModalA11y.ts); no bespoke Escape handlers. [→](docs/architecture/invariants.md#modal-accessibility)
- **Overlays are portalled, not z-indexed** — any `backdrop-filter` / `transform` ancestor caps the stacking context; z-100+ goes through `createPortal(…, document.body)`. Layer scale is documented at the top of [`src/app.css`](src/app.css). [→](docs/architecture/invariants.md#overlays-must-be-portalled-not-just-given-a-big-z-index)
- **Right panels are flex siblings, not overlays** — the center column carries `min-w-0`. [→](docs/architecture/invariants.md#right-panels-are-flex-siblings-not-overlays)
- **Process-wide offline mode** — every outbound HTTP path checks `offline::is_offline()` first. [→](docs/architecture/invariants.md#process-wide-offline-mode)
- **Non-frontend control surfaces go through [`player_actions`](src-tauri/crates/app/src/player_actions.rs)** — tray, media keys and MPD must not re-derive the advance + emit sequence. [→](docs/architecture/invariants.md#non-frontend-control-surfaces-go-through-player_actions)
- **New player-bar action** — lands in the "⋯" overflow menu first, promoted only when usage warrants it. [→](docs/architecture/invariants.md#adding-a-new-player-bar-action)
- **Plugins** — loaded at runtime, distributed from separate repos, blake3-pinned by the _registry_; published manifest strings use `*_i18n` siblings; `ui` plugins return a JSON descriptor and get only redacted library reads; `canvas` fan-out is fail-soft. [→](docs/architecture/invariants.md#plugins) · [full surface](docs/features/plugins.md)

## Feature catalogue

Names in `commands/`, `audio/` and `src/components/` are predictable — read the file. For anything that isn't obvious from the name, these are the deep dives:

| Area         | Doc                                                                                                     | Covers                                                                                                                                                                   |
| ------------ | ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Playback     | [`playback.md`](docs/features/playback.md)                                                              | decoder + DSD pipeline, crossfade (static / smart / dynamic), gapless, ReplayGain, EQ, speed, network pre-load, WASAPI exclusive, spectrum visualizer, A-B repeat, queue |
| Library      | [`library.md`](docs/features/library.md)                                                                | scanner + watcher, folder covers, local artist images, search + filters, tag editor, ratings, duplicates, import, history, multi-artist split                            |
| Playlists    | [`playlists.md`](docs/features/playlists.md) · [`smart-playlists.md`](docs/features/smart-playlists.md) | CRUD, sorting, auto-covers, M3U, Daily Mix + On Repeat generators, rule tree                                                                                             |
| Integrations | [`integrations.md`](docs/features/integrations.md)                                                      | Deezer, Last.fm, TheAudioDB, lyrics providers + editor, artist overrides, Discord RPC, OS notifications, scrobbling                                                      |
| Plugins      | [`plugins.md`](docs/features/plugins.md)                                                                | WASM host + sandbox, store, options, `source` / `metadata` / `ui` / `canvas` worlds, Web Radio + offline catalogue                                                       |
| UI & UX      | [`ui.md`](docs/features/ui.md)                                                                          | layout, 5 skins × 14 themes, immersive view, Canvas, cover slideshow, artist hero, mini-player, Wrapped, profiles, onboarding, settings, updater, backups                |
| LAN servers  | [`dlna.md`](docs/features/dlna.md) · [`mpd.md`](docs/features/mpd.md)                                   | opt-in MediaServer and MPD control surface                                                                                                                               |

## Conventions

- **Conventional commits**, enforced locally by husky `commit-msg` → `bunx commitlint --edit`. Config in `.commitlintrc.cjs` (header ≤ 100, kebab-case scopes). Subject stays **lowercase** — not sentence/start/pascal/upper case.
- **PR labels** are automatic (`.github/workflows/label-pr.yml`): `scope:*` by path, `type:*` from the title prefix, `size:*` from the diff.
- **Never hand-tag a release.** release-please owns version bumps across [`package.json`](package.json) (canonical), [`src-tauri/crates/app/tauri.conf.json`](src-tauri/crates/app/tauri.conf.json), [`src-tauri/Cargo.toml`](src-tauri/Cargo.toml) + `Cargo.lock`. Tag push drives bundles, the signed updater manifest and the downstream AUR / winget / copr / apt dispatches. Beta channel + full procedure: [`docs/RELEASING.md`](docs/RELEASING.md#beta-channel).
- **Flatpak sources are generated and go stale silently** — a lockfile bump without regenerating [`packaging/flatpak/generated/`](packaging/flatpak/generated/) fails inside Flathub's offline sandbox, not in CI. `check-sources.py` guards coverage per PR; note the `bun.lock` vs npm-lockfile split when pinning a dependency. [→](docs/RELEASING.md#flatpak-sources-are-generated-and-they-go-stale-silently)
- **Issue + PR templates** live under `.github/`.

## Language

UI copy ships in **17 locales** via i18next — `fr` (source of truth), `en`, `es`, `de`, `it`, `nl`, `pt`, `pt-BR`, `ru`, `tr`, `id`, `ja`, `ko`, `zh-CN`, `zh-TW`, `ar`, `hi`. Strings in `src/i18n/locales/<code>.json`; `index.ts` sets `document.documentElement.dir` so Arabic renders RTL. The legacy `kr` code stays accepted as an alias for `ko` (a startup migration rewrites stored preferences). The README is in English.

`fallbackLng: "en"` is set, but the convention is that **every locale carries every key** — no language-mixing in the UI. When you add a key, propagate it to all 17 files (a small Python script with `json.load`/`dump` + `ensure_ascii=False, indent=2` preserves the formatting).

Keep verbatim across locales: brand tokens (`WaveFlow`, `Last.fm`, `Deezer`, `ReplayGain`, `LRCLIB`, `BPM`), smart-playlist family names (`Daily Mix`, `On Repeat` — Spotify and Apple Music keep theirs untranslated in every market, and translating ours would split the user's mental model), and i18next `{{placeholder}}` tokens.
