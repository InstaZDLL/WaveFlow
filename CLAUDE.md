# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

WaveFlow is a local music player desktop app built with **Tauri 2 + React 19 + TypeScript + Vite** and a **bun** toolchain. Spotify / Apple Music-inspired UI on top of a Rust audio engine.

This file only covers the cross-cutting rules Claude needs in every conversation. **For per-feature deep dives (algorithms, schema, flow diagrams) read the relevant page under [`docs/features/`](docs/README.md)** — that's the source of truth when the overview here isn't enough.

## Development Commands

```bash
# Install dependencies
bun install

# Run the Tauri desktop app in development mode (Vite + Rust backend)
bun run tauri dev

# Build the production desktop app
bun run tauri build

# Frontend only
bun run dev           # Vite dev server (no Tauri shell)
bun run typecheck     # tsc --noEmit
bun run lint          # eslint
bun run build         # tsc + Vite prod build

# Rust backend
cargo check --manifest-path src-tauri/Cargo.toml --all-targets
cargo test  --manifest-path src-tauri/Cargo.toml
```

## Architecture

### Frontend (`src/`)

React 19 + TypeScript. Entry: `src/main.tsx` → `src/App.tsx`. Vite dev server on port 1420.

- **Contexts** (mounted as provider tree in `App.tsx`): `ThemeContext`, `PlayerContext`, `LibraryContext`, `PlaylistContext`, `ProfileContext`. `PageScrollContext` is mounted lower (in `AppLayout`) and exposes the main scrollable area to virtualized tables — single page-driven scrollbar.
- **Hooks** wrapping each context: `useTheme`, `usePlayer`, `useLibrary`, `usePlaylist`, `useProfile`, `usePageScroll`.
- **Tauri wrappers** (`src/lib/tauri/`): typed `invoke()` per backend command. Frontend uses camelCase, backend uses snake_case.
- **Views**: `HomeView`, `LibraryView`, `PlaylistView`, `AlbumDetailView`, `ArtistDetailView`, `LikedView`, `HistoryView`, `StatisticsView`, `WrappedView`, `SettingsView`, etc.
- **Layout**: Apple-Music-style sidebar (Ma musique + Playlists), TopBar with search, PlayerBar at bottom, right-edge panels (`NowPlayingPanel` / `QueuePanel` / `LyricsPanel`) mutex'd via `PlayerContext`.

A second `WebviewWindow` (label `mini`, `?mini=1` route) ships the always-on-top mini-player — see [`docs/features/ui.md`](docs/features/ui.md#mini-player).

### Backend (`src-tauri/`)

Rust / Tauri 2. Entry: `src-tauri/src/main.rs` → `lib.rs`.

- **Commands** (`src-tauri/src/commands/`): organized by domain — `library.rs`, `playlist.rs`, `smart_playlists.rs`, `track.rs`, `browse.rs`, `player.rs`, `scan.rs`, `edit.rs`, `profile.rs`, `analysis.rs`, `deezer.rs`, `similar.rs`, `lyrics.rs`, `stats.rs`, `wrapped.rs`, `integration.rs`, `maintenance.rs`, `app_info.rs`, `radio.rs`, `mood_radio.rs`, `duplicates.rs`, `preferences.rs`, `share.rs`, `changelog.rs`, etc. All registered in `lib.rs` via `generate_handler![]`.
- **External API clients** (crate-root modules): `deezer.rs` (public Deezer, no auth), `lastfm.rs` (`artist.getInfo`, user API key required). Both use `reqwest` with `rustls-tls`.
- **Audio engine** (`src-tauri/src/audio/`): 3-thread lock-free architecture — `decoder.rs` (symphonia + rubato), `output.rs` (cpal callback on a dedicated thread, SPSC `rtrb` ring buffer), `state.rs` (`SharedPlayback` with atomics, no locks in hot path), `analytics.rs` (tokio task for `play_event` writes + auto-advance), `crossfade.rs`, `eq.rs`, `spectrum.rs`, `wasapi_exclusive.rs` (Windows-only opt-in), and `dsd/` (in-house DSF/DFF parser + DSD→PCM converter, Symphonia doesn't decode DSD). Deep dive: [`docs/features/playback.md`](docs/features/playback.md).
- **DLNA / UPnP MediaServer** (`src-tauri/src/dlna/`): worker thread → axum HTTP server + SSDP announcer. Opt-in (`app_setting['dlna.enabled']`, default OFF). See [`docs/features/dlna.md`](docs/features/dlna.md).
- **OS media controls** (`media_controls.rs`): souvlaki bridge → SMTC / MPRIS / MediaRemote. Initialized post-window (needs HWND on Windows).
- **Discord Rich Presence** (`discord_presence.rs`): named-pipe IPC, opt-in `app_setting['integrations.discord_rpc']` (default ON). See [`docs/features/integrations.md`](docs/features/integrations.md#discord-rich-presence).
- **Queue** (`queue.rs`): persistent queue with fill / advance / shuffle (Fisher-Yates) / restore.
- **Smart playlists** (`smart_playlists/`): Daily Mix generator + composite cover renderer. See [`docs/features/smart-playlists.md`](docs/features/smart-playlists.md).
- **Database**: per-profile SQLite via sqlx + a global `app.db` for the profile list and app-wide settings (`app_setting`).

## Cross-cutting rules (always apply)

These bite you if you ignore them — they're the contract the rest of the codebase is built on.

- **Tauri commands**: `#[tauri::command]` in `commands/*.rs`, registered in `lib.rs::generate_handler![]`, called from React with `invoke("command_name", { args })`. Frontend camelCase, backend snake_case.
- **Profile-scoped pool**: `state.require_profile_pool().await?` — every command that touches user data goes through this. The shared `app.db` is for the profile list + cross-profile settings (Last.fm key, Discord opt-in, offline mode, backup config).
- **Persistence**: per-profile settings live in `profile_setting` (key-value, typed). Pattern: `INSERT ... ON CONFLICT DO UPDATE`. App-wide settings live in `app_setting` with the same shape.
- **Events**: backend emits Tauri events (`player:state`, `player:position`, `player:track-changed`, `player:queue-changed`, `player:error`, `player:ab-loop`, `player:spectrum`, `track:updated`, `library:rescanned`, `scan:progress`, `lyrics:updated`, …). Frontend listens via `listen()` from `@tauri-apps/api/event`.
- **Audio callback is hot**: the cpal callback (and the WASAPI exclusive thread) MUST NOT allocate, lock, or log. Only `rtrb::Consumer` reads + `Atomic*` loads. All heavy work (EQ, ReplayGain, resampling, FFT, BLAKE3) runs on the decoder thread before samples reach the SPSC ring.
- **Migrations are immutable once merged**: sqlx records a SHA-384 checksum in `_sqlx_migrations.checksum` at apply time, so editing a merged migration crashes every existing install at boot with `"migration <id> was previously applied but has been modified"`. For any schema evolution, **create a new dated migration** `YYYYMMDDhhmmss_<slug>.sql`. Same rule for `migrations/app/`. **Line-ending drift is a non-event** — [`db::migration_heal`](src-tauri/src/db/migration_heal.rs) reconciles stored checksums against the compiled-in migrator before each `Migrator::run`: when the stored hash matches the LF or CRLF variant of the same SQL (Windows `core.autocrlf=true` regression), it silently rewrites the row to the canonical hash and logs a warning. A real SQL change still panics, because neither LF nor CRLF normalization will rescue it.
- **Virtual scroll everywhere**: TrackTable uses `@tanstack/react-virtual` for 6000+ track performance. Virtualized tables consume `usePageScroll()` for the scroll element instead of nesting their own `overflow-y-auto` — drives a single Spotify-style scrollbar.
- **Multi-artist queries**: the scanner splits `"A, B"` on `", " / "; "` into individual `artist` rows linked via `track_artist`. Queries rebuild the display string via `GROUP_CONCAT` over `track_artist` ordered by `position`. `ArtistLink` accepts parallel `artist_name` + `artist_ids` strings so every contributor is individually clickable. New track queries must follow the same join pattern.
- **Album grouping = `(canonical_title, album_artist_id)`**: [`scan.rs::upsert_album`](src-tauri/src/commands/scan.rs) keys on the album artist (Album Artist tag → `is_compilation` → primary artist fallback). `album.is_compilation` is sticky and `merge_implicit_compilations` collapses ≥ 3 distinct-artist same-title rows into "Various Artists" after every scan. `edit.rs` re-runs `upsert_album` with the OLD album's Album Artist / compilation flags so renames don't re-split. Deep dive: [`docs/features/library.md`](docs/features/library.md#album-grouping).
- **Single writer to SQLite**: WAL mode allows concurrent reads but only one writer. Big import paths (`scan_folder_inner`, `edit.rs::update_track_tags`) wrap work in `pool.begin()` + commit every 200 rows. Upsert helpers (`upsert_artwork` / `upsert_artist` / `upsert_album` / `upsert_genre`) take `&mut sqlx::SqliteConnection` so they participate in the open transaction — never a pool clone mid-tx.
- **File-write safety on Windows**: any command that rewrites an audio file (`edit::update_track_tags`, `save_lyrics`, `set_track_rating`) MUST pause playback first when the engine reports the edited track as `current_track_id` — lofty's `save_to_path` needs an exclusive handle on Windows. Re-hash with blake3 + update `track.file_hash` after the write so the scanner's `(mtime, size)` fast path stays addressable.
- **Modal accessibility**: every modal calls [`useModalA11y(isOpen, onClose)`](src/hooks/useModalA11y.ts) — Escape-close, Tab focus trap, focus restoration. Container gets `role="dialog"` + `aria-modal="true"` + `aria-labelledby` (stable heading id) or `aria-label` (conditional heading). Don't roll bespoke `useEffect` Escape handlers.
- **Right panels are flex siblings, not overlays**: `NowPlayingPanel` / `QueuePanel` / `LyricsPanel` are mounted as flex children of the outer row in `AppLayout`. The center column has `min-w-0` so wide tables collapse instead of pushing the panel off-screen.
- **Process-wide offline mode**: every outbound HTTP path (Deezer, Last.fm, similar, LRCLIB) checks `offline::is_offline()` first and short-circuits to an empty payload or cache. Persisted in `app_setting['network.offline_mode']`. Treat new HTTP code paths the same way.
- **Adding a new player-bar action**: default it into the overflow ("⋯") menu via [`MoreActionsMenu`](src/components/player/MoreActionsMenu.tsx) first; promote to primary only when usage warrants it; add a Settings pin toggle if both modes make sense. See [`docs/features/ui.md`](docs/features/ui.md#player-bar-layout).

## Feature catalogue

One-liners + doc pointer. For everything else read the actual file in `commands/` or `audio/` — names are predictable.

### Playback ([`docs/features/playback.md`](docs/features/playback.md))

A-B repeat · crossfade (static / smart-album-aware / dynamic-tempo-aware) · gapless · ReplayGain · normalize · mono · 6-band peaking EQ (RBJ biquads, ±12 dB, 20 presets) · playback speed 0.5×–2× (resampler-shift, pitch follows) · DSD → PCM (256-tap Blackman-Harris FIR) · WASAPI Exclusive opt-in (Windows) with transparent fallback to cpal shared · spectrum visualizer (2048-pt FFT, opt-in) · output device persistence + cpal 0.17 friendly-name disambiguation · radio (seed + similar artists + BPM filter) · mood radio (focus/chill/workout/party/sleep) · sleep timer · TXXX:UNSYNCEDLYRICS fallback for MP3 K-Pop/J-Pop rips · sidecar `.lrc` / `.txt` auto-discovery next to the audio file or inside a sibling `Lyrics/` folder (case-insensitive, `.lrc` wins over `.txt`, runs before LRCLIB so no network hit when the rip ships its own lyrics) · word-level karaoke lyrics (Enhanced LRC + TTML parse, mot-à-mot capture in the editor).

### Library ([`docs/features/library.md`](docs/features/library.md))

Scanner with parallel BLAKE3 extraction + transactional commit + fs-watcher silent rescan + `scan:progress` toast · folder-cover fallback (cover/folder/front/albumart…) · local artist images (`artist.jpg` or `<artist_name>.jpg` resolved up to 3 parent dirs, prioritised over Deezer; `rescan_local_artist_images` backfills existing libraries) · advanced search (FTS5 + structured filters: genre, year, BPM, duration, format, Hi-Res, liked) · tag editor round-trips through lofty + DB · track ratings (POPM round-trip, half-step UI) · duplicate detection (BLAKE3 grouping) · folder removal + drag-and-drop import · listening history (`HistoryView` with month scrubber) · album grouping with sticky compilation flag (see cross-cutting rules above).

### UI ([`docs/features/ui.md`](docs/features/ui.md))

14-preset theme system ([`THEME_PRESETS`](src/lib/themes.ts)) — 6 light (Émeraude · Midnight · Sunset · Lavender · Crimson · Ocean) + 8 dark (Émeraude · OLED · Midnight · Sunset · Lavender · Crimson · Ocean · Neon), each declaring an OKLCH accent palette + ambient bg + optional `surfaceDark` / `surfaceDarkElevated` overrides. `applyTheme` writes CSS vars on `<html>`; Tailwind v4's `@theme inline` block remaps `bg-emerald-*` and `bg-surface-dark*` utilities to those vars so a swap re-tints the entire app without component edits. Inline bootstrap script in `index.html` paints the right `dark` class + `data-theme` + ambient bg before React mounts so a dark default doesn't flash white. View-Transitions radial reveal on click, with try/catch fallback to plain `setTheme` for WebKitGTK builds that throw synchronously. Picker lives in Settings → Appearance alongside [`PlayerBarLayoutCard`](src/components/views/settings/PlayerBarLayoutCard.tsx). Player bar with per-button visibility + cover-click action all driven from one Settings → Appearance panel ([`PlayerBarLayoutCard`](src/components/views/settings/PlayerBarLayoutCard.tsx)) backed by [`usePlayerBarLayout`](src/hooks/usePlayerBarLayout.ts) — Mini-player, Immersive, Lyrics, Queue, Device, A-B, Sleep, EQ presets, Audio-quality strip all toggleable, plus the cover thumbnail can be set to `immersive` (default, opens fullscreen) / `now_playing` (Spotify-style: toggles the right panel) / `none`. EQ presets share an `EqPresetPanel` body between the primary popover (when pinned) and the "⋯" overflow menu inline. Audio-quality footer ([`AudioQualityFooter`](src/components/player/AudioQualityFooter.tsx)) renders a compact source-specs strip with a Hi-Res pill; hover opens [`AudioPipelinePopover`](src/components/player/AudioPipelinePopover.tsx) — Source / Processing chips (`DSD → PCM` / `Resample` / `Downmix` / `EQ` / `ReplayGain` / `Normalize` / `Mono` / `Speed`) / Output, with a `Bit-perfect` pill when nothing is touching the stream · immersive Now Playing overlay · mini-player (`?mini=1` second webview, 280×380 default, always-on-top, cover-derived gradient background, position + size restored on next launch from `app_setting['mini_player.bounds']` with offscreen-after-monitor-disconnect fallback to the bottom-right anchor) · karaoke fullscreen lyrics · lyrics editor (plain + Musicolet-style synced) · first-run onboarding (Lokal-style multi-step wizard: welcome → language → profile → localOnly → folder → Last.fm → scan → done; profile step renames the auto-created "Default" profile in place via [`rename_profile`](src-tauri/src/commands/profile.rs); modal is `flex flex-col max-h-[calc(100vh-2rem)]` with the body scrollable so the progress bar and action bar stay visible on 1080p displays; latched per-profile, re-evaluates against the new profile's libraries on switch via `LibraryContext.loadedProfileId`) · profile create + rename + delete (`ProfileSelectorModal` Netflix-style "Manage" toggle, atomic `DELETE … AND (SELECT COUNT(*) FROM profile) > 1` guard against losing the last profile, active profile refused to delete; rename is safe against the active profile since only `app.db` is touched) · Settings split into seven ARIA tablist categories (library / playback / integrations / appearance / data / shortcuts / diagnostics) — only the active panel mounts · WaveFlow Wrapped (year-in-review story overlay + shareable PNG; Home banner gated by [`useWrappedBannerVisibility`](src/hooks/useWrappedBannerVisibility.ts) — three modes `auto` (default — visible only during Wrapped season, Dec 1 → Jan 31, matching Spotify's release window) / `always` / `never`, plus a per-year dismiss button on the banner that writes `profile_setting['wrapped.dismissed_year']`; toggle UI in [`WrappedBannerCard`](src/components/views/settings/WrappedBannerCard.tsx) under Settings → Appearance) · Now Playing share card · configurable keyboard shortcuts · persistent zoom (VS Code style shortcuts) · full-width music views (no `max-w-*` cap on listing views) · single-instance lock ([`tauri-plugin-single-instance`](https://crates.io/crates/tauri-plugin-single-instance) first plugin in `lib.rs`).

### Playlists ([`docs/features/playlists.md`](docs/features/playlists.md), [`docs/features/smart-playlists.md`](docs/features/smart-playlists.md))

Playlist sort dropdown (custom / title / artist / album / recently added / duration / filename — non-custom modes are display-only via `Intl.Collator`, never touch `playlist_track.position`; filename sorts on the cross-platform basename of `track.file_path`) · auto-cover (Spotify-style 2×2 grid composite from first 4 tracks; manual upload flips `cover_is_auto=0`) · smart playlists (Daily Mix family + recursive boolean rule tree via `CustomRules`, v1 flat → v2 tree auto-migration) · M3U import/export.

### Integrations ([`docs/features/integrations.md`](docs/features/integrations.md))

Deezer enrichment (pictures, covers, fans — cached 30 days in `deezer_artist` / `deezer_album` in `app.db`, hashes point into shared `metadata_artwork/<blake3>.jpg` so artwork renders offline) · Last.fm (bios, similar artists, scrobbler) · Discord RPC · DLNA / UPnP MediaServer ([`docs/features/dlna.md`](docs/features/dlna.md)).

### Preferences & maintenance

Autostart + close-to-tray + scan-on-start ([`commands/preferences.rs`](src-tauri/src/commands/preferences.rs)) · profile export/import (`.waveflow` zip via `commands/profile_io.rs` — bundles `data.db` + `artwork/` + manifest, plus the shared `metadata_artwork/**` Deezer cache when `app_setting['backup.include_metadata_artwork']` is on (default), runs `PRAGMA wal_checkpoint(TRUNCATE)` on the active profile first) · auto-backup ([`backup.rs`](src-tauri/src/backup.rs) tokio task that shares `profile_io::write_archive` with manual export; the shared cache is bundled only into the first archive of each pass to avoid N× duplication across profiles) · stats JSON export · embedded changelog (parsed from `git log` at compile time in `build.rs`).

## Conventions

- **Conventional commits** enforced locally via husky `commit-msg` → `bunx commitlint --edit`. Config in `.commitlintrc.cjs` (header ≤ 100, kebab-case scopes). `prepare: husky` auto-installs the hook on `bun install`. Subject must NOT be sentence-case / start-case / pascal-case / upper-case — keep it lowercase.
- **PR labels**: `.github/workflows/label-pr.yml` auto-applies `scope:*` (path-based via `actions/labeler`), `type:*` (parsed from PR title prefix), `size:*` (from diff line count).
- **Release & distribution**: release-please owns version bumps and tags. Bumping a version means editing **three** manifests in lockstep + regenerating `Cargo.lock` via `cargo check`: [`package.json`](package.json) (canonical), [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json), [`src-tauri/Cargo.toml`](src-tauri/Cargo.toml). The release-please PR handles all of this automatically — **never hand-tag**. Tag push fires [`release.yml`](.github/workflows/release.yml) which builds Linux/Windows/macOS bundles + signed updater manifest, then explicitly `gh workflow run`s downstream `aur.yml` / `winget.yml` / `copr.yml` / `apt-publish.yml` (the last one pushes the `.deb` to the Buildkite Packages apt registry at `instazdll/waveflow`) (GitHub silently drops `release: published` events when created by `GITHUB_TOKEN`). Full procedure: [`docs/RELEASING.md`](docs/RELEASING.md).
- **Issue + PR templates**: `.github/ISSUE_TEMPLATE/` ships YAML form templates (bugs + features). `.github/pull_request_template.md` reminds contributors of the `bun run typecheck` / `bun run lint` / `cargo check` triple-check before opening.

## Language

The README is in English. The app ships UI copy in **17 locales** via i18next — `fr` (source of truth), `en`, `es`, `de`, `it`, `nl`, `pt`, `pt-BR`, `ru`, `tr`, `id`, `ja`, `ko`, `zh-CN`, `zh-TW`, `ar`, `hi`. Strings in `src/i18n/locales/<code>.json`. `index.ts` sets `document.documentElement.dir` per language so Arabic renders RTL automatically. The legacy `kr` code is still accepted as an alias for back-compat (early builds shipped `kr` instead of the ISO 639-1 `ko`); a one-time migration rewrites stored `kr` preferences to `ko` at startup.

`fallbackLng: "en"` is set, but the project convention is **every locale carries every key** so the experience stays coherent without language-mixing. When you add a new key, propagate it to all 17 locales (a small Python script using `json.load`/`dump` with `ensure_ascii=False, indent=2` keeps the existing formatting intact). Brand tokens (`WaveFlow`, `Last.fm`, `Deezer`, `ReplayGain`, `LRCLIB`, `BPM`) stay verbatim across locales. Preserve i18next `{{placeholder}}` interpolation tokens unchanged.
