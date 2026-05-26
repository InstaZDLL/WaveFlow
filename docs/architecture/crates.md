# Crate layout

`src-tauri/` is a Cargo workspace with two members:

```text
src-tauri/
├── Cargo.toml         (virtual workspace root)
├── Cargo.lock
├── migrations/        (sqlx schema, per-profile + app-level)
├── vendor/            (glib 0.18.5 RUSTSEC backport)
└── crates/
    ├── core/          (waveflow-core — portable business logic)
    │   └── src/
    │       ├── analysis.rs            (peak / loudness / ReplayGain / BPM)
    │       ├── artwork/               (thumbnail pipeline + shared metadata cache)
    │       ├── audio_format/dsd/      (DSF + DFF parsing, 1-bit → 24-bit PCM)
    │       ├── domain/                (Track, Album, Artist, Playlist, Profile, Library DTOs)
    │       ├── error.rs               (CoreError + CoreResult)
    │       ├── metadata/              (Deezer / Last.fm / LRCLIB HTTP clients)
    │       ├── repository/            (storage traits + sqlite implementations)
    │       │   ├── library.rs / playlist.rs / profile.rs / track.rs
    │       │   └── sqlite/            (Sqlite* impls of each trait)
    │       ├── scanner/               (file extract + upsert helpers, no orchestration)
    │       └── smart_playlists/       (Daily Mix, On Repeat, custom rule eval, cover composer)
    └── app/           (waveflow — Tauri 2 application)
        ├── Cargo.toml                 (produces the `waveflow` binary)
        ├── tauri.conf.json
        ├── capabilities/  icons/  build.rs
        └── src/
            ├── audio/                 (real-time cpal + rtrb pipeline, EQ, WASAPI exclusive)
            ├── commands/              (#[tauri::command] handlers, thin over core)
            ├── db/                    (per-profile pool wiring + migration_heal)
            ├── dlna/                  (MediaServer worker thread)
            ├── discord_presence.rs    (Rich Presence named-pipe client)
            ├── media_controls.rs      (souvlaki bridge → SMTC / MPRIS)
            ├── notifications.rs       (Tauri notification plugin bridge)
            ├── paths.rs               (AppPaths via tauri::AppHandle + dirs)
            ├── scrobbler.rs           (Last.fm scrobble worker)
            ├── state.rs               (AppState — held by Tauri)
            └── watcher.rs             (notify-driven fs watch)
```

The full workspace builds with `cargo check --workspace --all-targets --manifest-path src-tauri/Cargo.toml`. CI runs the same command (`.github/workflows/ci.yml`).

## What goes in `waveflow-core`

Anything that could run inside an axum handler in the future `waveflow-server` (RFC-001 §6.2) without dragging Tauri or `cpal` along. Specifically:

- **Domain types** — the DTOs the UI sees. Plain `serde::Serialize`/`Deserialize` plus an opt-in `sqlx::FromRow` derive (via the `sqlite` feature) so the same struct backs `query_as` calls without a parallel row type.
- **Repository traits** — `ProfileRepository`, `LibraryRepository`, `PlaylistRepository`, `TrackRepository`. Each lives in `repository/<entity>.rs` with a SQLite implementation under `repository/sqlite/`. A future `repository/postgres/` will be the server's counterpart.
- **HTTP clients** for the third-party metadata sources we enrich the local library with (`metadata/{deezer,lastfm,lrclib}.rs`). Pure `reqwest` over rustls; no Tauri.
- **Scanner helpers** — every pure function the file-walker calls per track (`scanner/extract.rs` for the lofty / blake3 / cover-extraction side, `scanner/upserts.rs` for the SQL writes and the album-grouping policy). The Tauri-aware orchestrator (`scan_folder_inner` + the `scan:progress` emit) stays in `app/commands/scan.rs` until a future `ScannerEventSink` decoupling.
- **Smart-playlist engine** — Daily Mix generator, On Repeat regen, custom rule evaluator, cover composer. The `PathsContext` struct decouples the engine from `AppPaths`; the app constructs one from its `state.paths`.
- **Audio analysis** — per-track peak / loudness / ReplayGain / BPM (`analysis.rs`). Symphonia-based, no `cpal`.
- **Audio format conversion** — DSD parser + decimating FIR (`audio_format/dsd/*`). The real-time playback pipeline still uses this from `app/src/audio/crossfade.rs`.
- **Artwork pipeline** — the shared blake3-keyed metadata cache + the SIMD thumbnail variants (`artwork/{metadata,thumbnails}.rs`).
- **Error type** — `CoreError`. The desktop's `AppError` wraps it via `#[from] CoreError` so `?` flattens automatically across the boundary.

## What stays in `waveflow` (`crates/app/`)

Anything tied to the Tauri runtime, the real-time audio engine, or the desktop OS:

- **Every `#[tauri::command]`** — even when the body is a thin call into a core function. The IPC bridge contract is desktop-specific.
- **Real-time audio engine** — `audio/{decoder,output,engine,crossfade,eq,resampler,spectrum,state,wasapi_exclusive,analytics}.rs`. The `cpal` callback and the WASAPI exclusive thread must not allocate / log / lock; the surrounding decoder + state machinery only makes sense alongside them.
- **OS media controls** — souvlaki (`media_controls.rs`), Discord Rich Presence named-pipe client (`discord_presence.rs`), system notification plugin bridge (`notifications.rs`).
- **DLNA / UPnP MediaServer** — `dlna/` is integrated as a worker thread driven by the Tauri runtime.
- **Filesystem watcher** — `watcher.rs` wires `notify` events into `library:rescanned` Tauri events.
- **DB pool wiring** — `db/{app_db,profile_db,migration_heal}.rs`. Migrations themselves live at `src-tauri/migrations/` and are compiled in by `sqlx::migrate!(...)` from app; moving migrations into core is a later cleanup once nothing app-side needs to point at them with a relative path.
- **Paths** — `paths.rs::AppPaths` derives the on-disk layout from `tauri::AppHandle` + `dirs::data_dir()`. The server will have its own path resolver.
- **Tray, single-instance, updater, mini-player WebviewWindow** — all platform-specific Tauri wiring.

## Re-export shims

Several files in `crates/app/src/` are one-line re-exports of code that moved to core, kept in place so existing `crate::*` imports across the app keep resolving without churn:

- `app/src/thumbnails.rs` → `waveflow_core::artwork::thumbnails`
- `app/src/metadata_artwork.rs` → `waveflow_core::artwork::metadata`
- `app/src/analysis.rs` → `waveflow_core::analysis`
- `app/src/smart_playlists.rs` → `waveflow_core::smart_playlists::*` (submodules re-exported individually so the `cover` / `custom` / `generator` / `on_repeat` paths still work)

Likewise the domain types in `commands/{track,playlist,library,profile}.rs` and `commands/scan.rs::canonical_name` stay reachable through `pub use waveflow_core::...` lines at the top of their original files.

These shims are cosmetic — they keep the diff small while the migration lands. Future cleanup PRs can collapse them by walking the call sites.

## When to split `waveflow-core` into its own repo

Not now. RFC-001 §5 plans for `waveflow-core` to live in this repo until its public API stabilises (estimated 2-3 months of `waveflow-server` consumption). At that point it moves out in a single `git filter-repo` pass and starts publishing to crates.io; both `waveflow` and `waveflow-server` switch from `path = "../core"` to a git or crates.io dependency.

Until then: anything `waveflow-server` would need lives at `waveflow_core::*` already.

## Feature flags on `waveflow-core`

| Flag       | What it enables                                                                                                                                                    |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `sqlite`   | `sqlx`'s `sqlite` + `runtime-tokio` features (so `repository::sqlite::*` compiles) and the `sqlx::FromRow` derives on the domain types (via `cfg_attr`).           |

The desktop crate opts in with `features = ["sqlite"]`. The future server will add a `postgres` feature with the same shape (`sqlx/postgres`, FromRow derives gated on it). Trait definitions in `repository/` itself stay storage-agnostic — only their implementations live behind a feature flag.

## Tauri config + bundle paths

The `tauri-build` crate reads `tauri.conf.json` from the directory containing the building `Cargo.toml`, so after the workspace split it had to move next to `crates/app/Cargo.toml`:

```text
src-tauri/crates/app/
├── tauri.conf.json   (frontendDist + licenseFile up two extra levels)
├── build.rs          (changelog generator + capabilities writer)
├── capabilities/     (default.json + generated updater.json)
└── icons/
```

The Tauri CLI looks for `tauri.conf.json` via Cargo discovery, which can't see the file from the project root in this layout. A small wrapper at [`scripts/tauri.mjs`](../../scripts/tauri.mjs) injects `--config src-tauri/crates/app/tauri.conf.json` for the three subcommands that load it (`dev` / `build` / `bundle`) and passes everything else through unchanged; `package.json::scripts.tauri` points at it. Contributors keep typing `bun run tauri build`.
