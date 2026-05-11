# UI & UX

The UI is React 19 + Tailwind CSS 4. The provider tree is mounted in [`App.tsx`](../../src/App.tsx); the layout shell is in [`AppLayout.tsx`](../../src/components/layout/AppLayout.tsx).

## Layout

Three-column flex row:

```
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

## System tray

Quick playback controls (Play/Pause, Previous, Next, Quitter). Close-to-tray is the default close behaviour — the `WindowEvent::CloseRequested` handler hides the window unless the tray "Quitter" item armed `QuitGate`. Tray ID is `waveflow`.

## Statistics view

[`StatisticsView.tsx`](../../src/components/views/StatisticsView.tsx) projects from `play_event`:

- KPIs (total listening time, distinct tracks/artists/albums, completion rate)
- GitHub-contributions-style yearly heatmap ([`Heatmap.tsx`](../../src/components/views/statistics/Heatmap.tsx)) — 53×7 grid pinned to the past 12 months regardless of the period selector, intensity bucketed in quartiles against the local max so the gradient stays meaningful for both light and heavy listeners. Reuses `stats_listening_by_day` with `range="1y"`; no new backend command.
- Listening-by-day and listening-by-hour bar charts
- Top tracks / artists / albums for the selected window (7d / 30d / 90d / 1y / all)

## Width & containers

Music browsing views (Home, Library, Playlist, Album, Artist, Liked, Recent, Statistics) render **full width** inside the center column — no `max-w-*` cap. The `p-8` gutter on the page scroller ([`AppLayout.tsx`](../../src/components/layout/AppLayout.tsx)) is the only horizontal breathing room. On a 2.5K display the table area gains ~800 px over the previous `max-w-6xl mx-auto` constraint.

Form-style views (Settings, About, Feedback) keep `max-w-4xl` because dense forms read better with a comfortable line length.

Track tables themselves are **borderless** — no `rounded-2xl border bg-white` card wrapper. The page already provides the visual frame; nesting another card just shrinks every row by ~80 px and breaks the Spotify-style "rows on the page" feel. The column-header `border-b` is the only separator between header and rows.

## Performance

- **Virtual scroll** — `@tanstack/react-virtual` on every long list (tracks, queue, playlist contents, statistics rows). Tables share the page-level scroller via [`usePageScroll()`](../../src/hooks/usePageScroll.ts) and compute `scrollMargin` from the parent's offset so the virtualiser knows where its content begins. Single Spotify-style scrollbar, no nested overflow.
- **Image cache** — in-memory LRU (`lib/imageCache.ts`) for `convertFileSrc` results so the same artwork URL isn't recomputed on every render.
- **Thumbnails** — 1× and 2× covers generated by [`thumbnails.rs`](../../src-tauri/src/thumbnails.rs) with `fast_image_resize` (SIMD AVX/SSE/NEON depending on host) and served via the asset protocol.

## Player-bar visibility toggles

A handful of niche playback features live behind a per-profile visibility toggle so the player bar stays uncluttered for typical users. Both default to **off** and opt in from Settings → Lecture:

| Setting key           | Button                       | Default |
|-----------------------|------------------------------|---------|
| `ui.show_sleep_timer` | Moon icon (sleep timer menu) | off     |
| `ui.show_ab_loop`     | Repeat icon (A-B loop)       | off     |

The PlayerBar listens to `waveflow:sleep-timer-visibility` / `waveflow:ab-loop-visibility` window events dispatched by the Settings toggle so the icons appear / disappear without a polling loop.

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
- Profile switch closes the current per-profile pool and opens the new one — UI reactively re-fetches via every `*Provider` watching `activeProfile.id`.

### Export / import (`.waveflow` archive)

[`commands/profile_io.rs`](../../src-tauri/src/commands/profile_io.rs) packages a profile into a single `.waveflow` (zip) file containing `manifest.json` + `data.db` + the per-profile `artwork/` directory. Settings → Stockage exposes both buttons.

- **Export:** the active-profile path runs `PRAGMA wal_checkpoint(TRUNCATE)` first so the bundled DB captures every committed page (otherwise a busy WAL would leave the archive holding a partial snapshot). The CPU-bound zip work runs on `tokio::task::spawn_blocking`.
- **Import:** always allocates a fresh profile row — never overwrites — then extracts the archive under `profiles/<new_id>/`. Failures roll the row back so a half-imported profile doesn't survive the error. Once extracted, the new pool is opened once so any pending sqlx migrations replay before the user switches to it.
- **Out of scope:** the shared `app.db` (Last.fm key, Discord opt-in, `network.offline_mode`) belongs to the install, not the profile. The shared `metadata_artwork/` cache (Deezer pictures, etc.) is re-fetchable so we skip it to keep archives small.
- **Manifest:** `archive_version` (currently `1`) gates compatibility — a future schema-incompatible bump refuses imports rather than silently corrupting the new profile. `app_version` and the source profile name / id are recorded for diagnostics.

## Onboarding

[`OnboardingModal`](../../src/components/common/OnboardingModal.tsx) prompts new profiles to point at a music folder. The decision is **latched once per profile** via `profile_setting['onboarding.dismissed']`, so the modal never reappears after a "configure later" choice — even if the library stays empty.

The modal only appears when the profile is fully resolved AND the library is empty AND the flag isn't set, preventing flashes during boot transitions.

## Auto-updater

Tauri updater plugin with a signed update flow. The update banner offers "Install now" without forcing a relaunch interruption. **Wired in release builds only** — in `tauri dev` the local source tree wouldn't have a signed manifest to fetch, so the plugin would just spam errors. See [`lib.rs`](../../src-tauri/src/lib.rs) for the `#[cfg(not(debug_assertions))]` gate.
