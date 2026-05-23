# UI & UX

The UI is React 19 + Tailwind CSS 4. The provider tree is mounted in [`App.tsx`](../../src/App.tsx); the layout shell is in [`AppLayout.tsx`](../../src/components/layout/AppLayout.tsx).

## Layout

Three-column flex row:

```bash
┌──────────┬──────────────────────────────┬──────────┐
│ Sidebar  │ Center column                │ Right    │
│          │  ┌────────────────────────┐  │ panel    │
│  - Home  │  │ TopBar (search + nav)  │  │          │
│  - Lib   │  ├────────────────────────┤  │ Now      │
│  - …     │  │                        │  │ Playing  │
│  - Pls   │  │  Scrollable content    │  │   or     │
│          │  │                        │  │ Queue    │
│          │  └────────────────────────┘  │   or     │
│          │                              │ Lyrics   │
├──────────┴──────────────────────────────┴──────────┤
│ PlayerBar (bottom, full width)                     │
└────────────────────────────────────────────────────┘
```

The right panel is **a flex sibling** of the center column, not an overlay — opening it shrinks the content area Spotify-style. The center column has `min-w-0` so wide tables collapse instead of pushing the panel off-screen. Only one of the three right-panels is mounted at a time (mutex via `PlayerContext`).

## Panels

- [`NowPlayingPanel`](../../src/components/layout/NowPlayingPanel.tsx) — large artwork, clickable artists, "About the artist" section populated from the Deezer + Last.fm caches, and a "Next in queue" teaser with an "Open queue" link that hands the right slot off to `QueuePanel`. Lightbox on cover click.
- [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) — current queue with drag reorder, jump-to-track, clear queue.
- [`LyricsPanel`](../../src/components/layout/LyricsPanel.tsx) — synced or static lyrics with auto-scroll.
- [`NowPlayingChevronTab`](../../src/components/layout/NowPlayingChevronTab.tsx) — right-edge floating tab visible only when no panel is open.

## Immersive Now Playing

[`FullscreenNowPlaying`](../../src/components/player/FullscreenNowPlaying.tsx) is an Apple-Music-style overlay (`fixed inset-0 z-100`) that turns the current track into the focal point: huge centred cover, large title + clickable artist + album, the same `PlaybackControls` / `ProgressBar` / `VolumeControl` the bottom bar uses, and a like toggle. Background is a blurred copy of the artwork (with a 55% black wash over it) so the view stays visually anchored to the track without any extra theming work.

Two entry points in the [`PlayerBar`](../../src/components/player/PlayerBar.tsx): clicking the cover in the bottom bar (mirrors Spotify) or the dedicated Maximize2 icon next to the lyrics toggle. Closes on Escape or the X button. State is local to the bar — no `PlayerContext` involvement because nothing else needs to know about the overlay.

## Mini-player

[`MiniPlayerApp`](../../src/MiniPlayerApp.tsx) + [`MiniPlayer`](../../src/components/views/MiniPlayer.tsx) ship a Spotify-style always-on-top widget. Launched from the picture-in-picture button in the PlayerBar via [`lib/miniPlayer.ts::openMiniPlayer`](../../src/lib/miniPlayer.ts).

- **Window** — second `WebviewWindow` (label `mini`), 280×380 with `decorations: false` (we render our own top bar) and `alwaysOnTop: true`. Anchored bottom-right of the primary monitor (`currentMonitor` → physical size ÷ scale factor → logical px) with a 24 px edge margin so the OS taskbar / Dock isn't covered. Hides the main window on open; the mini's Maximize button restores it and closes the mini.
- **Routing** — same Vite bundle, branched in [`main.tsx`](../../src/main.tsx) on `?mini=1` so the mini boots into a stripped-down provider tree (`Theme + Profile + Player` only — no `Library` / `Playlist` since the widget never browses).
- **Cover-derived background** — [`lib/dominantColor.ts`](../../src/lib/dominantColor.ts) draws the artwork onto a 64×64 canvas, samples every 4th pixel, skips near-monochrome runs (white margins, black bars) so the average reflects the real hue, and produces a 3-stop gradient applied to the window background.
- **Hover overlay controls** — shuffle / prev / play (white round Spotify-style) / next / repeat fade in over the cover; idle state shows just the artwork.
- **Drag region** — `data-tauri-drag-region` on the central dot strip, plus an explicit `getCurrentWindow().startDragging()` `onMouseDown` as a belt-and-suspenders fallback for the Windows hit-test races. Requires `core:window:allow-start-dragging` in the capability (not in `core:default`).
- **Pin toggle** — runtime `setAlwaysOnTop(bool)`; emerald when active.
- **Interactive seek bar** — slim white bar at the bottom, click/drag to scrub. Same `pointer capture` + local `dragMs` pattern as the main `ProgressBar`. Thumb + timestamps fade in on hover so the idle widget stays minimal.
- **Capabilities** — the mini-player's window label is added to [`capabilities/default.json`](../../src-tauri/capabilities/default.json) so it inherits every command the main window has access to (no duplicated capability file, no per-window permission pruning).

## Splash screen

To hide the cold-start delay (Windows SmartScreen / Defender scanning every freshly-extracted DLL on the very first launch after install, plus the `setup()` chain in [`lib.rs`](../../src-tauri/src/lib.rs) — opening `app.db` + running migrations, creating the default profile, cold-initialising cpal/WASAPI), the main window is created with `"visible": false` and a small secondary window (`label: "splashscreen"`, 360×240, opaque `#121212`, decorations off, always-on-top, off the taskbar) shows a WaveFlow logo + indeterminate progress bar while the backend boots and the React bundle parses.

The static HTML lives in [`public/splash.html`](../../public/splash.html) (no JS, inline SVG logo, single CSS animation) so it paints the instant the WebView2 process spawns. The splash → main handoff is **driven from the backend** — the frontend's [`ReadySignal`](../../src/components/common/ReadySignal.tsx) component emits `app://ready` after React's first commit (via `useEffect`, not `requestAnimationFrame` — WebKitGTK 2.52 suspends rAF callbacks while a window is `visible: false`), and [`lib.rs`](../../src-tauri/src/lib.rs)'s setup installs an `app.listen("app://ready", …)` listener that calls `reveal_main_close_splash`: show main first, set focus, then close the splash so the desktop is never visible between the two. A 15 s safety-net timer + bounded retry loop (10 attempts, 250 ms backoff) revives the handoff if the event never arrives. The mini-player webview branches out via `?mini=1` and skips the dance.

Why backend-driven: v1.1.0 ran the handoff entirely in `main.tsx` via `requestAnimationFrame` + IPC `window.show()` + `splash.close()`. On Linux WebKitGTK 2.52+ the heavy first-launch init (migrations + DB pool + WebKit profile dir) raced the rAF window, the show()/close() could fire on a non-ready webview, and the user was stuck on an eternal splash (issue #42). Native-side ownership + an explicit "DOM committed" signal is robust to that race.

Splash window background is opaque on purpose: `"transparent": true` forces an alpha-capable EGL config that some WebKitGTK builds reject, doubling the EGL failure surface (see also the AppImage incompatibility note for WebKitGTK 2.52+).

## System tray

Quick playback controls (Play/Pause, Previous, Next, Quitter). Close-to-tray is the default close behaviour — the `WindowEvent::CloseRequested` handler hides the window unless the tray "Quitter" item armed `QuitGate`. Tray ID is `waveflow`.

## Statistics view

[`StatisticsView.tsx`](../../src/components/views/StatisticsView.tsx) projects from `play_event`:

- KPIs (total listening time, distinct tracks/artists/albums, completion rate)
- GitHub-contributions-style yearly heatmap ([`Heatmap.tsx`](../../src/components/views/statistics/Heatmap.tsx)) — 53×7 grid pinned to the past 12 months regardless of the period selector, intensity bucketed in quartiles against the local max so the gradient stays meaningful for both light and heavy listeners. Reuses `stats_listening_by_day` with `range="1y"`; no new backend command.
- Listening-by-day and listening-by-hour bar charts
- Top tracks / artists / albums for the selected window (7d / 30d / 90d / 1y / all)
- **JSON export** — `export_stats_json(range, target_path)` ([`commands/stats.rs`](../../src-tauri/src/commands/stats.rs)) bundles the active range's overview + top 100 tracks/artists/albums + listening-by-day + listening-by-hour into a versioned (`schema_version: 1`) pretty-printed JSON file. The Rust side writes the file directly via `spawn_blocking` so we don't depend on `tauri-plugin-fs` just to round-trip a string. Frontend trigger is the Download button next to the range selector in the header.

## WaveFlow Wrapped

[`WrappedView.tsx`](../../src/components/views/WrappedView.tsx) is a year-in-review experience modelled on Spotify Wrapped, built **entirely from local `play_event` rows** — no network call, no external service. Three backend commands in [`commands/wrapped.rs`](../../src-tauri/src/commands/wrapped.rs):

- `available_wrapped_years()` — distinct years that have at least one play event, sorted descending. Used to gate the HomeView banner and populate the in-overlay year picker.
- `get_wrapped(year)` — bundles every aggregate into a single payload: overview (plays / minutes / unique tracks / artists / albums), top 10 tracks + artists + top 5 albums (reusing the row shapes from `commands/stats.rs` so the artwork resolver works unchanged), per-month + per-hour histograms, most active day, mood profile, first listen of the year, and longest consecutive-day listening streak.
- `wrapped_current_year()` — server-side `Local::now().year()` so the frontend doesn't depend on the JS `Date` for the fallback default.

Year bounds are computed in **local time** (Jan 1 00:00 → Dec 31 23:59:59, exclusive upper) so a play at 23:59 on Dec 31 lands in the right year regardless of UTC offset. The mood profile uses listening-weighted averages (weight = `listened_ms`) so a 4 min play of a fast track counts ~16× a 15 s skip of a slow one — otherwise a hate-skip collection would skew the BPM mean. The energy label is derived from BPM buckets server-side (`< 80 → chill`, `< 110 → warm`, `< 135 → groove`, `< 160 → energetic`, else `fire`) but is localised on the frontend via a fixed dictionary so we never ship copy from Rust.

The streak walks the distinct-day list once and tracks the longest run of dates that increment by exactly one day. Bounded at 366 rows per year — no fancy gaps-and-islands SQL needed.

Frontend overlay (`fixed inset-0 z-100`, same pattern as `FullscreenNowPlaying`) ships 10–12 auto-advancing slides at ~6.5 s each. Slides without data are filtered out before the rotation starts — no analysed tracks → no mood slide; no streak ≥ 2 days → no streak slide — so a brand-new profile with three plays still gets a coherent (if short) experience. Top-of-screen progress segments + space-to-pause + arrow-key navigation match Instagram / Snapchat story conventions. The HomeView entry point is a gradient banner above the Mood Radio grid, hidden entirely when `available_wrapped_years` returns an empty list.

### Shareable PNG

The Share button in the overlay top bar opens a two-action menu: **Save as PNG** (native save dialog → file on disk) and **Copy image** (clipboard via `navigator.clipboard.write` + `ClipboardItem`). Both go through [`lib/wrappedCard.ts`](../../src/lib/wrappedCard.ts), a pure Canvas 2D renderer that produces a 1080×1920 portrait PNG mirroring the overlay's visual style — radial-gradient backdrop sampled from the same accent palette, year + total minutes as marquee elements, top 5 tracks with cover thumbnails, mood + streak strip, "Powered by WaveFlow" footer. Text uses the WebView's native font stack so we don't ship a font file with the bundle. The "save" path serialises the PNG bytes through the IPC channel ([`save_share_image(bytes, target_path)`](../../src-tauri/src/commands/share.rs), shared with the Now Playing card) and writes via `spawn_blocking` — no `tauri-plugin-fs` dependency. The "copy" path stays in the browser and works on Chromium-based WebView (Edge on Windows, WKWebView on macOS); WebKitGTK on Linux historically refused image/png clipboard writes, so the error is surfaced rather than silently no-op'd.

## Now Playing share card

Same Save / Copy pattern as Wrapped, but applied to the **currently-playing track**. The Share button in the [`FullscreenNowPlaying`](../../src/components/player/FullscreenNowPlaying.tsx) top bar generates a 1080×1080 square PNG via [`lib/nowPlayingCard.ts`](../../src/lib/nowPlayingCard.ts) — the cover artwork is drawn full-bleed under a dark wash for the background, then again as a centred 580 px tile with rounded corners + drop shadow, followed by title + artist + album text. The bottom of the card carries a thin accent strip in the artwork's dominant colour (sampled via the existing [`lib/dominantColor.ts`](../../src/lib/dominantColor.ts)) so each card visually nods to its source cover. Backend writes go through the same `save_share_image` Tauri command as Wrapped — the IPC channel is feature-agnostic so future share card flows (album, playlist) can reuse it without new commands. Disabled when no track is playing.

## Width & containers

Music browsing views (Home, Library, Playlist, Album, Artist, Liked, Recent, Statistics) render **full width** inside the center column — no `max-w-*` cap. The `p-8` gutter on the page scroller ([`AppLayout.tsx`](../../src/components/layout/AppLayout.tsx)) is the only horizontal breathing room. On a 2.5K display the table area gains ~800 px over the previous `max-w-6xl mx-auto` constraint.

Form-style views (Settings, About, Feedback) keep `max-w-4xl` because dense forms read better with a comfortable line length.

Track tables themselves are **borderless** — no `rounded-2xl border bg-white` card wrapper. The page already provides the visual frame; nesting another card just shrinks every row by ~80 px and breaks the Spotify-style "rows on the page" feel. The column-header `border-b` is the only separator between header and rows.

## Performance

- **Virtual scroll** — `@tanstack/react-virtual` on every long list (tracks, queue, playlist contents, statistics rows). Tables share the page-level scroller via [`usePageScroll()`](../../src/hooks/usePageScroll.ts) and compute `scrollMargin` from the parent's offset so the virtualiser knows where its content begins. Single Spotify-style scrollbar, no nested overflow.
- **Image cache** — in-memory LRU (`lib/imageCache.ts`) for `convertFileSrc` results so the same artwork URL isn't recomputed on every render.
- **Thumbnails** — 1× and 2× covers generated by [`thumbnails.rs`](../../src-tauri/src/thumbnails.rs) with `fast_image_resize` (SIMD AVX/SSE/NEON depending on host) and served via the asset protocol.

## Player-bar layout

Right side of [`PlayerBar`](../../src/components/player/PlayerBar.tsx) is the highest-pressure real estate in the UI — every new feature wants an icon there. To keep the bar from running out of width on narrow windows, controls cluster by frequency:

| Tier         | Controls                                                                   | Where                                                                                                                                        |
| ------------ | -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| **Primary**  | Lyrics, Queue, Device picker, "⋯", Volume, **Mini-player**, **Fullscreen** | Always visible. Spotify-style right cluster (mini-player + fullscreen) sits after volume                                                     |
| **Overflow** | Playback speed (slider + presets), A-B loop, Sleep timer (panel)           | [`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx) — "⋯" popover; the trigger itself is hidden when nothing inside is left |
| **Pinnable** | A-B loop, Sleep timer (promote to primary)                                 | Toggle in Settings → Lecture (see below)                                                                                                     |

When adding a new player-bar action: default it into the overflow menu first — promote to primary only when usage data or user feedback warrants it. If both placements make sense, expose a pin toggle. The "⋯" trigger auto-hides when its menu would be empty.

**Playback speed** lives inside [`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx) (range slider + five presets) rather than a dedicated bar button — it's used too rarely to deserve a permanent slot. When speed ≠ 1×, the "⋯" trigger surfaces a compact `1.25×` badge in emerald (same corner as the sleep-timer countdown — the countdown wins when both are active). See [playback / Playback speed](playback.md#playback-speed-05--2) for the backend side.

### Pin toggles

A-B loop and Sleep timer are **always available** — they live in the "⋯" overflow menu by default. The pin toggles let frequent users promote them to a primary slot on the bar so they're one click away. Both default to **off**:

| Setting key           | Pinned button rendered in primary slot | Default |
| --------------------- | -------------------------------------- | ------- |
| `ui.show_sleep_timer` | Moon icon (sleep timer menu)           | off     |
| `ui.show_ab_loop`     | Repeat icon (A-B loop)                 | off     |

When a pin is OFF, the entry stays in the overflow menu and the sleep-timer countdown badge surfaces on the "⋯" trigger itself so the user keeps live feedback while the timer is armed. The PlayerBar listens to `waveflow:sleep-timer-visibility` / `waveflow:ab-loop-visibility` window events dispatched by the Settings toggle so the layout re-renders without a polling loop.

## Keyboard shortcuts

Action ↔ key bindings live in [`src/lib/shortcuts.ts`](../../src/lib/shortcuts.ts) (12 actions, defaults like `Space` → play/pause, `←`/`→` → previous/next, `M` → mute, `S` → shuffle, `R` → repeat, `L` → toggle lyrics, `Shift+L` → like). [`useGlobalShortcuts`](../../src/hooks/useGlobalShortcuts.ts) is mounted once in [`AppLayout`](../../src/components/layout/AppLayout.tsx) and attaches a single `window.keydown` listener that dispatches against `PlayerContext`. Listener skips when the focus target is `INPUT` / `TEXTAREA` / `contenteditable` so typing in a search box doesn't toggle shuffle.

User overrides are stored per-profile in `profile_setting['ui.shortcuts']` as a JSON object containing only customised actions — defaults stay implicit, so future default tweaks land for any binding the user hasn't touched. Settings → Raccourcis clavier ([`ShortcutsCard`](../../src/components/views/settings/ShortcutsCard.tsx)) captures keys in capture-phase so the rebind UI doesn't fire the global handler. Conflicts auto-resolve by stealing the combo from whoever previously owned it. AboutView reads the same setting and re-renders on the `waveflow:shortcuts-changed` window event.

## Theming & motion

- **Dark mode** — animated radial transition via the [View Transitions API](https://developer.mozilla.org/en-US/docs/Web/API/View_Transitions_API). Falls back to an instant swap when unsupported.
- **`prefers-reduced-motion`** respected for the radial transition and for animated SVGs.
- **Single-click play** — optional Settings toggle; the default is double-click to mirror Apple Music / Finder.

## i18n

17 locales in [`src/i18n/locales/`](../../src/i18n/locales): `fr` (source of truth), `en`, `es`, `de`, `it`, `nl`, `pt`, `pt-BR`, `ru`, `tr`, `id`, `ja`, `kr` (registered as `ko` + `kr` alias), `zh-CN`, `zh-TW`, `ar`, `hi`. Auto-detected at first launch from the OS locale, switchable from Settings.

There is **no per-key fallback**, so every locale must include every key. [`index.ts`](../../src/i18n/index.ts) sets `document.documentElement.dir` per language so Arabic renders RTL automatically.

Non-French locales were bulk-translated from `fr.json` through DeepL with explicit music-player context, then post-processed to keep brand tokens (`WaveFlow`, `Last.fm`, `Deezer`, `ReplayGain`, `LRCLIB`, `BPM`) verbatim and preserve i18next `{{placeholder}}` interpolation.

To add a language:

1. Create `src/i18n/locales/xx.json` (same structure as `fr.json`)
2. Import it in `src/i18n/index.ts` and add to `SUPPORTED_LANGUAGES`
3. It appears in the Settings selector automatically

## Profiles

Per-profile isolated database (libraries, playlists, settings, play history); shared metadata cache across profiles (artwork, Deezer / Last.fm metadata, lyrics).

- The `profile` table lives in `app.db` along with `app_setting['app.last_profile_id']`.
- Boot flow: if no profiles exist, create "Default"; otherwise activate `last_profile_id`, falling back to the most-recently-used profile if it points to a deleted row.
- Profile switch closes the current per-profile pool and opens the new one — UI reactively re-fetches via every `*Provider` watching `activeProfile.id`. [`LibraryContext`](../../src/contexts/LibraryContext.tsx) also exposes `loadedProfileId` (the id its `libraries` array was last fetched for) so consumers like the onboarding gate in [`AppLayout`](../../src/components/layout/AppLayout.tsx) wait for a fresh fetch instead of evaluating against the previous profile's data. `refresh()` snapshots the active profile id before its `await` and drops late writes when the user has since switched.

### Create / delete

[`ProfileSelectorModal`](../../src/components/common/ProfileSelectorModal.tsx) hosts the lifecycle:

- **Create** → "+" tile in the select view → name + colour picker → backend [`create_profile`](../../src-tauri/src/commands/profile.rs) reserves the row, materialises `profiles/<id>/`, and runs the initial migration. The freshly-created profile is auto-activated.
- **Delete** → Netflix-style "Manage" toggle (pencil ↔ check) in the top-left corner reveals a red trash badge on every non-active profile; tapping it opens a destructive confirmation view. Backend [`delete_profile`](../../src-tauri/src/commands/profile.rs) refuses the active profile and the last remaining profile. The guard is **atomic**: a single SQL statement (`DELETE FROM profile WHERE id = ? AND (SELECT COUNT(*) FROM profile) > 1`) couples the "must not be last" predicate with the mutation so two concurrent deletes can never empty the table. Disambiguation between "not found" and "last profile" is handled on the failure path. After the row is removed, `profiles/<id>/` is wiped from disk and `app.last_profile_id` is cleared if it pointed to the deleted profile.

### Export / import (`.waveflow` archive)

[`commands/profile_io.rs`](../../src-tauri/src/commands/profile_io.rs) packages a profile into a single `.waveflow` (zip) file containing `manifest.json` + `data.db` + the per-profile `artwork/` directory. Settings → Stockage exposes both buttons.

- **Export:** the active-profile path runs `PRAGMA wal_checkpoint(TRUNCATE)` first so the bundled DB captures every committed page (otherwise a busy WAL would leave the archive holding a partial snapshot). The CPU-bound zip work runs on `tokio::task::spawn_blocking`.
- **Import:** always allocates a fresh profile row — never overwrites — then extracts the archive under `profiles/<new_id>/`. Failures roll the row back so a half-imported profile doesn't survive the error. Before the sqlx migrator runs, [`normalise_migration_checksums`](../../src-tauri/src/commands/profile_io.rs) rewrites `_sqlx_migrations.checksum` for every version present in both the archive and the local migrator — older builds checked out migration files with CRLF endings (Windows `core.autocrlf=true` + no `.gitattributes` lock) so their stored SHA-384 differs from the same SQL re-hashed today, even though the DDL is identical. A `.gitattributes` at repo root now pins `*.sql` / `*.rs` / `*.ts` / etc. to LF so future archives stay byte-stable. Once normalised, the new pool is opened once so any pending sqlx migrations replay before the user switches to it. An archive whose `_sqlx_migrations` lists a version unknown to the local migrator is rejected — that means the export came from a newer build.
- **Out of scope:** the shared `app.db` (Last.fm key, Discord opt-in, `network.offline_mode`) belongs to the install, not the profile. The shared `metadata_artwork/` cache (Deezer pictures, etc.) is re-fetchable so we skip it to keep archives small.
- **Manifest:** `archive_version` (currently `1`) gates compatibility — a future schema-incompatible bump refuses imports rather than silently corrupting the new profile. `app_version` and the source profile name / id are recorded for diagnostics.

### Auto-backup

Opt-in scheduled mirror of the manual export so the user's playlists / likes / ratings / history survive a SQLite corruption or disk failure. Implementation in [`backup.rs`](../../src-tauri/src/backup.rs):

- **Config** lives in `app_setting` (install-wide, not per-profile): `backup.enabled` (bool, default OFF), `backup.interval_days` (1-90, default 7), `backup.folder` (string; empty = default `<app_data>/waveflow/backups/`), `backup.retention` (1-50, default 5 — per profile), `backup.last_run_at` (epoch ms).
- **Loop** is a single tokio task started once at boot ([`spawn_backup_loop`](../../src-tauri/src/backup.rs)). When disabled, parks on a `tokio::sync::Notify` (zero cost) until the user toggles. When enabled, computes the next deadline as `last_run_at + interval_days * 86_400_000` and uses `tokio::select!` between a sleep and the same `Notify` so config changes wake it without waiting for the old sleep to expire.
- **Pass** ([`run_one_backup`](../../src-tauri/src/backup.rs)) iterates every row in `profile`, calls the shared [`profile_io::write_archive`](../../src-tauri/src/commands/profile_io.rs) (pub-crate-ified from the manual-export path so the two stay bit-compatible), and applies retention per profile (`<sanitized-name>-*.waveflow` sorted by mtime, oldest beyond `retention` deleted). The active profile gets a `PRAGMA wal_checkpoint(TRUNCATE)` first; inactive profiles are already cold on disk (the pool ran a checkpoint at switch / shutdown).
- **Failure isolation:** per-profile errors are logged but don't abort the pass — one corrupt profile shouldn't block backups of the healthy ones.
- **Commands** in [`commands/backup.rs`](../../src-tauri/src/commands/backup.rs): `get_backup_config`, `set_backup_config` (also signals the loop), `run_backup_now`. UI is [`BackupCard`](../../src/components/views/settings/BackupCard.tsx) in Settings → Stockage right after the manual export/import.

## Settings categories

[`SettingsView`](../../src/components/views/SettingsView.tsx) is split into seven Lokal-style horizontal tabs rendered as a proper ARIA `role="tablist"` at the top of the page (keyboard-navigable, `aria-selected` per panel):

| Tab            | Houses                                                                                                 |
| -------------- | ------------------------------------------------------------------------------------------------------ |
| `library`      | Library folders, scan-on-start, file watcher                                                           |
| `playback`     | EQ, crossfade, ReplayGain, normalisation, WASAPI exclusive, mono                                       |
| `integrations` | Last.fm, Discord RPC, Deezer enrichment, DLNA media server                                             |
| `appearance`   | Theme, accent colour, language, zoom, immersive layout                                                 |
| `data`         | Profile export / import, auto-backup, statistics export, offline                                       |
| `shortcuts`    | Per-action keyboard rebinder ([`ShortcutsCard`](../../src/components/views/settings/ShortcutsCard.tsx)) |
| `diagnostics`  | Log folder reveal, recent log tail, app info                                                           |

Only one panel mounts at a time, so heavy sub-views (EQ visualiser, backup card, shortcuts editor) don't run their effects until the user opens that tab.

## Onboarding

[`OnboardingModal`](../../src/components/common/OnboardingModal.tsx) walks new profiles through a Lokal-style multi-step wizard. Steps in order:

1. **welcome** — branding + privacy pitch.
2. **language** — picker over [`SUPPORTED_LANGUAGES`](../../src/i18n/index.ts); persists immediately so the rest of the wizard renders in the chosen locale.
3. **profile** — name the auto-created "Default" profile in place via [`rename_profile`](../../src-tauri/src/commands/profile.rs). Safe against the active profile since only `app.db` is touched; the per-profile pool keeps its open handle. Skipping the rename (input unchanged) avoids the backend round-trip entirely.
4. **localOnly** — explainer that the library never leaves the device unless the user opts into Last.fm / Discord later.
5. **folder** — calls [`pickFolder`](../../src/lib/tauri/dialog.ts) to select a music root and creates the first library entry.
6. **lastfm** — optional Last.fm API key + secret pairing (skippable). Status lives in [`integration.rs`](../../src-tauri/src/commands/integration.rs).
7. **scan** — kicks off the initial scan and surfaces progress.
8. **done** — success state with a "Open the app" button.

The modal is laid out as `flex flex-col max-h-[calc(100vh-2rem)]` with the progress bar pinned to the top (`shrink-0`), the step body in the middle (`overflow-y-auto flex-1 min-h-0`), and the action bar pinned to the bottom (`shrink-0`). Without those constraints the wizard's tallest steps (Last.fm with 4 inputs + button) push the header and footer off-screen on 1080p displays.

The decision is **latched once per profile** via `profile_setting['onboarding.dismissed']`, so the wizard never reappears after a "configure later" / completed run — even if the library stays empty.

The modal only opens when:

- the profile is fully resolved (no boot-time flicker),
- the [`LibraryContext`](../../src/contexts/LibraryContext.tsx) has refetched **for the new profile id** (`loadedProfileId === activeProfile.id`), so a switch from a populated profile to a brand-new empty one is detected with the new profile's data instead of the previous closure,
- the library is empty,
- `onboarding.dismissed` isn't set for this profile.

## Auto-updater

Tauri updater plugin with a signed update flow. The update banner offers "Install now" without forcing a relaunch interruption. **Wired in release builds only** — in `tauri dev` the local source tree wouldn't have a signed manifest to fetch, so the plugin would just spam errors. See [`lib.rs`](../../src-tauri/src/lib.rs) for the `#[cfg(not(debug_assertions))]` gate.
