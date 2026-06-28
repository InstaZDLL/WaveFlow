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

### View loading & code-splitting

Every full-page view (Home, Library, Liked, History, Playlist, Album/Artist/Genre detail, Statistics, Wrapped, Settings) is `React.lazy()`-loaded. The Suspense fallback in [`AppLayout`](../../src/components/layout/AppLayout.tsx) is [`ViewSuspenseFallback`](../../src/components/common/ViewSuspenseFallback.tsx) — a layout-shaped skeleton (`role="status"` / `aria-busy="true"`) instead of a spinner that read as a blank screen. To make the fallback rarely fire at all, AppLayout schedules a `requestIdleCallback` after first mount that warm-imports every lazy view module; once those imports resolve they're cached in the module registry, so a sidebar click usually skips Suspense entirely.

Per-view data fetches initialise their `isLoading` state to `true` (not `false`) so the first render paints a skeleton matching the view's shape rather than flashing the empty-state for the frame between mount and the first effect tick. Detail pages (Album/Artist/Genre) share [`DetailViewSkeleton`](../../src/components/common/DetailViewSkeleton.tsx); list-shaped pages use inline `<…Skeleton>` components colocated with their view file.

## Panels

- [`NowPlayingPanel`](../../src/components/layout/NowPlayingPanel.tsx) — large artwork, clickable artists, "About the artist" section populated from the Deezer + Last.fm caches, and a "Next in queue" teaser with an "Open queue" link that hands the right slot off to `QueuePanel`. Lightbox on cover click.
- [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) — current queue with drag reorder, jump-to-track, clear queue.
- [`LyricsPanel`](../../src/components/layout/LyricsPanel.tsx) — synced or static lyrics with auto-scroll.
- [`NowPlayingChevronTab`](../../src/components/layout/NowPlayingChevronTab.tsx) — right-edge floating tab visible only when no panel is open.

## Immersive view

[`ImmersiveView`](../../src/components/player/ImmersiveView.tsx) (`fixed inset-0 z-100`) is the Apple-Music-TV-style fullscreen view that turns the current track into the focal point: the cover hero + metadata + transport centred as one group on the left, and a **tabbed control panel on the right** — Lyrics / Queue (issue #328). Background is a blurred copy of the artwork (with a 65% black wash) so the view stays visually anchored without extra theming.

It merges what used to be two mutually-exclusive overlays (`FullscreenNowPlaying` + `FullscreenLyrics`) into one view, decomposed as: [`ImmersiveNowPlaying`](../../src/components/player/ImmersiveNowPlaying.tsx) (left column — cover/metadata/transport), [`ImmersiveSidePanel`](../../src/components/player/ImmersiveSidePanel.tsx) (right control panel — segmented Lyrics / Queue tabs) wrapping [`ImmersiveLyricsColumn`](../../src/components/player/ImmersiveLyricsColumn.tsx) (karaoke scroller) + [`ImmersiveQueueTab`](../../src/components/player/ImmersiveQueueTab.tsx) (now-playing + up-next, double-click to jump; reuses `player_get_queue` / `player_jump_to_index`, white-on-dark styled — the docked [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) keeps the theme-aware styling + drag-reorder), and [`ImmersiveShareButton`](../../src/components/player/ImmersiveShareButton.tsx) (share-as-PNG in the top bar). The orchestrator owns the shared close / share / panel-toggle chrome.

**One lyrics fetch, two consumers.** The lyrics state (three-tier fetch + parse + active-line / active-word tracking + import/refetch/clear, with every mid-flight staleness guard) lives in [`useTrackLyrics`](../../src/hooks/useTrackLyrics.ts). Both the side [`LyricsPanel`](../../src/components/layout/LyricsPanel.tsx) and `ImmersiveView` consume it, so the merged view never threads state from the side panel (the old `FullscreenLyrics` did, via a `createPortal` from `LyricsPanel`) — opening the immersive view no longer force-opens the right-edge panel. Both hook instances key on the live `currentTrack`, so the rare case of both surfaces open at once resolves the second `fetch_lyrics` from the backend cache.

**Two genuinely distinct interfaces** ([`useImmersivePrefs`](../../src/hooks/useImmersivePrefs.ts), per-profile; `merged_lyrics` + `use_native_fullscreen` both default **OFF** so the view is exactly the pre-#328 experience out of the box — each is opt-in) — the merged layout is an _option_, not a replacement:

- `immersive.merged_lyrics` ON + window ≥ 900 px → **merged**: now-playing left (~60%) + tabbed control panel right (~40%). The top-bar `PanelRight` toggle hides/shows the panel (left re-centres when hidden); the active tab (Lyrics / Queue) persists across hide/show. A clean lyrics miss keeps a discreet "No lyrics — Import / Fetch" CTA so the per-track relayout stays calm.
- pref OFF **or** window < 900 px → **classic**: now-playing fullscreen with a Mic2 button that flips to a lyrics-only fullscreen — the original behaviour, left intact (no control panel, no queue tab). The entry point (cover/now-playing vs lyrics button) picks the first side.

**Native fullscreen** (`immersive.use_native_fullscreen`, opt-in, default OFF) drives the OS window into real fullscreen on open via `getCurrentWindow().setFullscreen(true)`, capturing the prior maximized state in a ref and restoring it on close — so the OS chrome (title bar, taskbar) actually disappears. This needs the `core:window:allow-set-fullscreen` / `allow-is-fullscreen` / `allow-is-maximized` / `allow-maximize` permissions in [`capabilities/default.json`](../../src-tauri/crates/app/capabilities/default.json) (added with the feature — capabilities are compiled into the binary, so adding them requires a Rust rebuild). OFF keeps the in-window overlay for multi-monitor users who want the rest of their desktop visible. Escape (or the X) closes the view; exiting OS fullscreen another way (F11) is deliberately **decoupled** — it leaves the view as a windowed overlay rather than dismissing it. Both toggles live in [`ImmersiveViewCard`](../../src/components/views/settings/ImmersiveViewCard.tsx) under Settings → Appearance, and the feature is surfaced as a discreet tip on the onboarding "done" step.

Entry points in the [`PlayerBar`](../../src/components/player/PlayerBar.tsx): clicking the cover (mirrors Spotify) or the Immersive button, plus the Maximize2 icon in the side `LyricsPanel` header. `PlayerContext` exposes `immersiveOpen` + `immersiveInitialTab` + `openImmersive`/`closeImmersive`; the old `openFullscreenNowPlaying`/`openFullscreenLyrics`/`close*` names stay as back-compat aliases that all drive the one merged view.

**Transition hygiene** — the view paints a solid `bg-zinc-950` on the outer wrapper from the first frame; the `animate-fade-in` keyframe lives on the inner backdrop + foreground layers, not the wrapper. Without that opaque base the wrapper's own opacity ramp (0 → 1 over 300 ms) would let the page underneath bleed through during the transition.

**Skin neutrality** — the immersive view carries `role="dialog"` + a `shadow-2xl` cover, which the skins' modal / surface / text-colour chrome would otherwise capture (Liquid-light + Editorial repainted it as a light glass slab with dark, invisible text + washed-out controls; Lounge / Pulse flattened the cover backdrop). The root is tagged `data-immersive` + a nested `dark` context (so the shared `PlaybackControls` / `VolumeControl` / `ProgressBar` render their dark-theme variants over the always-dark backdrop), and every skin's relevant rules carry `:not([data-immersive]):not([data-immersive] *)` so the view renders identically across skins.

**Long titles scroll** — the now-playing title and the bottom `PlayerBar` title use [`MarqueeText`](../../src/components/common/MarqueeText.tsx): it measures overflow via a `ResizeObserver` and, only when the text actually overflows, glides it end-to-end and back (ping-pong with a pause at each extremity, `prefers-reduced-motion` respected) instead of truncating. Toggleable per-profile via [`ScrollTitlesCard`](../../src/components/views/settings/ScrollTitlesCard.tsx) under Settings → Appearance ([`useScrollLongTitles`](../../src/hooks/useScrollLongTitles.ts), `ui.scroll_long_titles`, default ON — off falls back to ellipsis truncation).

## Mini-player

[`MiniPlayerApp`](../../src/MiniPlayerApp.tsx) + [`MiniPlayer`](../../src/components/views/MiniPlayer.tsx) ship a Spotify-style always-on-top widget. Launched from the picture-in-picture button in the PlayerBar via [`lib/miniPlayer.ts::openMiniPlayer`](../../src/lib/miniPlayer.ts).

- **Window** — second `WebviewWindow` (label `mini`), default 280×380 with `decorations: false` (we render our own top bar) and `alwaysOnTop: true`. Hides the main window on open; the mini's Maximize button restores it and closes the mini.
- **Persistent bounds** — position + size are persisted in `app_setting['mini_player.bounds']` (JSON blob, machine-level) via debounced `onMoved` / `onResized` listeners in [`MiniPlayer.tsx`](../../src/components/views/MiniPlayer.tsx) (300 ms after the last gesture so SQLite isn't hammered at 60 Hz while dragging). On open, [`miniPlayer.ts::openMiniPlayer`](../../src/lib/miniPlayer.ts) restores the saved rectangle when it still overlaps an available monitor by at least 80 px on both axes (`availableMonitors()` check guards against monitor disconnects / resolution changes). Otherwise it falls back to anchoring bottom-right of the primary monitor (`currentMonitor` → physical size ÷ scale factor → logical px) with a 24 px edge margin so the OS taskbar / Dock isn't covered.
- **Routing** — same Vite bundle, branched in [`main.tsx`](../../src/main.tsx) on `?mini=1` so the mini boots into a stripped-down provider tree (`Theme + Profile + Player` only — no `Library` / `Playlist` since the widget never browses).
- **Cover-derived background** — [`lib/dominantColor.ts`](../../src/lib/dominantColor.ts) draws the artwork onto a 64×64 canvas, samples every 4th pixel, skips near-monochrome runs (white margins, black bars) so the average reflects the real hue, and produces a 3-stop gradient applied to the window background.
- **Hover overlay controls** — shuffle / prev / play (white round Spotify-style) / next / repeat fade in over the cover; idle state shows just the artwork.
- **Drag region** — `data-tauri-drag-region` on the central dot strip, plus an explicit `getCurrentWindow().startDragging()` `onMouseDown` as a belt-and-suspenders fallback for the Windows hit-test races. Requires `core:window:allow-start-dragging` in the capability (not in `core:default`).
- **Pin toggle** — runtime `setAlwaysOnTop(bool)`; emerald when active.
- **Up-next overlay** — a `ListMusic` toggle in the top bar slides a translucent up-next list over the content area (the top bar stays reachable so the toggle still closes it). Backed by the same `player_get_queue` + `player:queue-changed` subscription as the main [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) (seq-guarded refetch), it lists every track after `current_index`; clicking a row calls `player_jump_to_index`, and finished tracks drop off as the index advances. Compact (no artwork, just position · title · artist). Local-library only — gated on `!isSpotify` like the like button, since Spotify playback uses a different queue source.
- **Interactive seek bar** — slim white bar at the bottom, click/drag to scrub. Same `pointer capture` + local `dragMs` pattern as the main `ProgressBar`. Thumb + timestamps fade in on hover so the idle widget stays minimal.
- **Capabilities** — the mini-player's window label is added to [`capabilities/default.json`](../../src-tauri/capabilities/default.json) so it inherits every command the main window has access to (no duplicated capability file, no per-window permission pruning).

## Splash screen

To hide the cold-start delay (Windows SmartScreen / Defender scanning every freshly-extracted DLL on the very first launch after install, plus the `setup()` chain in [`lib.rs`](../../src-tauri/crates/app/src/lib.rs) — opening `app.db` + running migrations, creating the default profile, cold-initialising cpal/WASAPI), the main window is created with `"visible": false` and a small secondary window (`label: "splashscreen"`, 360×240, opaque `#121212`, decorations off, always-on-top, off the taskbar) shows a WaveFlow logo + indeterminate progress bar while the backend boots and the React bundle parses.

The static HTML lives in [`public/splash.html`](../../public/splash.html) (no JS, inline SVG logo, single CSS animation) so it paints the instant the WebView2 process spawns. The splash → main handoff is **driven from the backend** — the frontend's [`ReadySignal`](../../src/components/common/ReadySignal.tsx) component emits `app://ready` after React's first commit (via `useEffect`, not `requestAnimationFrame` — WebKitGTK 2.52 suspends rAF callbacks while a window is `visible: false`), and [`lib.rs`](../../src-tauri/crates/app/src/lib.rs)'s setup installs an `app.listen("app://ready", …)` listener that calls `reveal_main_close_splash`: show main first, set focus, then close the splash so the desktop is never visible between the two. A 15 s safety-net timer + bounded retry loop (10 attempts, 250 ms backoff) revives the handoff if the event never arrives. The mini-player webview branches out via `?mini=1` and skips the dance.

Why backend-driven: v1.1.0 ran the handoff entirely in `main.tsx` via `requestAnimationFrame` + IPC `window.show()` + `splash.close()`. On Linux WebKitGTK 2.52+ the heavy first-launch init (migrations + DB pool + WebKit profile dir) raced the rAF window, the show()/close() could fire on a non-ready webview, and the user was stuck on an eternal splash (issue #42). Native-side ownership + an explicit "DOM committed" signal is robust to that race.

Splash window background is opaque on purpose: `"transparent": true` forces an alpha-capable EGL config that some WebKitGTK builds reject, doubling the EGL failure surface (see also the AppImage incompatibility note for WebKitGTK 2.52+).

## System tray

Quick playback controls (Play/Pause, Previous, Next, Quitter). Close-to-tray is the default close behaviour — the `WindowEvent::CloseRequested` handler hides the window unless the tray "Quitter" item armed `QuitGate`. Tray ID is `waveflow`.

## Statistics view

[`StatisticsView.tsx`](../../src/components/views/StatisticsView.tsx) projects from `play_event`:

- KPIs (total listening time, distinct tracks/artists/albums, completion rate). Each card carries a stable id and the user can hide any of them from **Settings → Appearance** ([`StatsKpiVisibilityCard`](../../src/components/views/settings/StatsKpiVisibilityCard.tsx)) — persisted per-profile in `profile_setting['stats.hidden_kpis']` as a JSON array of ids, read via [`useHiddenKpis`](../../src/hooks/useHiddenKpis.ts) (window-event broadcast so the view re-reads without a remount). Default = nothing hidden. Motivated by the "Full-listen rate" KPI feeling judgemental, but applies uniformly to every KPI.
- GitHub-contributions-style yearly heatmap ([`Heatmap.tsx`](../../src/components/views/statistics/Heatmap.tsx)) — 53×7 grid pinned to the past 12 months regardless of the period selector, intensity bucketed in quartiles against the local max so the gradient stays meaningful for both light and heavy listeners. Reuses `stats_listening_by_day` with `range="1y"`; no new backend command.
- Listening-by-day and listening-by-hour bar charts
- Per-genre breakdown ([`TopGenres.tsx`](../../src/components/views/statistics/TopGenres.tsx)) — horizontal bars sized by `SUM(listened_ms)`, backed by `stats_top_genres(range, limit)` joining `play_event → track_genre → genre`. A multi-genre track credits every genre attached to it (intentional: a "Rock; Indie" play counts toward both).
- Top tracks / artists / albums for the selected window (7d / 30d / 90d / 1y / all)
- **JSON export** — `export_stats_json(range, target_path)` ([`commands/stats.rs`](../../src-tauri/crates/app/src/commands/stats.rs)) bundles the active range's overview + top 100 tracks/artists/albums/genres + listening-by-day + listening-by-hour into a versioned (`schema_version: 2` — v2 added `top_genres`) pretty-printed JSON file. The Rust side writes the file directly via `spawn_blocking` so we don't depend on `tauri-plugin-fs` just to round-trip a string. Frontend trigger is the Download button next to the range selector in the header.

## WaveFlow Wrapped

[`WrappedView.tsx`](../../src/components/views/WrappedView.tsx) is a year-in-review experience modelled on Spotify Wrapped, built **entirely from local `play_event` rows** — no network call, no external service. Three backend commands in [`commands/wrapped.rs`](../../src-tauri/crates/app/src/commands/wrapped.rs):

- `available_wrapped_years()` — distinct years that have at least one play event, sorted descending. Used to gate the HomeView banner and populate the in-overlay year picker.
- `get_wrapped(year)` — bundles every aggregate into a single payload: overview (plays / minutes / unique tracks / artists / albums), top 10 tracks + artists + top 5 albums (reusing the row shapes from `commands/stats.rs` so the artwork resolver works unchanged), per-month + per-hour histograms, most active day, mood profile, first listen of the year, and longest consecutive-day listening streak.
- `wrapped_current_year()` — server-side `Local::now().year()` so the frontend doesn't depend on the JS `Date` for the fallback default.

Year bounds are computed in **local time** (Jan 1 00:00 → Dec 31 23:59:59, exclusive upper) so a play at 23:59 on Dec 31 lands in the right year regardless of UTC offset. The mood profile uses listening-weighted averages (weight = `listened_ms`) so a 4 min play of a fast track counts ~16× a 15 s skip of a slow one — otherwise a hate-skip collection would skew the BPM mean. The energy label is derived from BPM buckets server-side (`< 80 → chill`, `< 110 → warm`, `< 135 → groove`, `< 160 → energetic`, else `fire`) but is localised on the frontend via a fixed dictionary so we never ship copy from Rust.

The streak walks the distinct-day list once and tracks the longest run of dates that increment by exactly one day. Bounded at 366 rows per year — no fancy gaps-and-islands SQL needed.

Frontend overlay (`fixed inset-0 z-100`, same pattern as `ImmersiveView`) ships 10–12 auto-advancing slides at ~6.5 s each. Slides without data are filtered out before the rotation starts — no analysed tracks → no mood slide; no streak ≥ 2 days → no streak slide — so a brand-new profile with three plays still gets a coherent (if short) experience. Top-of-screen progress segments + space-to-pause + arrow-key navigation match Instagram / Snapchat story conventions.

### Home banner visibility

The HomeView entry point is a gradient banner above the Mood Radio grid, gated by [`useWrappedBannerVisibility`](../../src/hooks/useWrappedBannerVisibility.ts) — three modes persisted in `profile_setting['wrapped.banner_visibility']`:

- **`auto`** (default) — shows the banner only during the **Wrapped season** (December 1 → January 31, local time), matching Spotify Wrapped's release cadence so the recap stays an event rather than permanent dashboard furniture. The rest of the year the banner is hidden but the WrappedView remains reachable.
- **`always`** — render whenever `available_wrapped_years` returns at least one year. Power-user opt-in for people who want their recap pinned year-round.
- **`never`** — never on Home. The view itself stays reachable.

The banner also exposes a per-recap-year dismiss button (the `X` in the top-right corner) that writes `profile_setting['wrapped.dismissed_year']` so a quick close hides the banner for that year only — next year's recap re-appears automatically. Mode is configured from Settings → Appearance via [`WrappedBannerCard`](../../src/components/views/settings/WrappedBannerCard.tsx). The card also surfaces the current season status (`seasonActive` / `seasonIdle`) when `auto` is selected so the user understands why the banner is or isn't on their Home right now.

The full banner stack — visibility check + `available_wrapped_years` length — collapses to nothing when either condition is unmet, so an empty library never paints the banner regardless of mode.

### Shareable PNG

The Share button in the overlay top bar opens a two-action menu: **Save as PNG** (native save dialog → file on disk) and **Copy image** (clipboard via `navigator.clipboard.write` + `ClipboardItem`). Both go through [`lib/wrappedCard.ts`](../../src/lib/wrappedCard.ts), a pure Canvas 2D renderer that produces a 1080×1920 portrait PNG mirroring the overlay's visual style — radial-gradient backdrop sampled from the same accent palette, year + total minutes as marquee elements, top 5 tracks with cover thumbnails, mood + streak strip, "Powered by WaveFlow" footer. Text uses the WebView's native font stack so we don't ship a font file with the bundle. The "save" path serialises the PNG bytes through the IPC channel ([`save_share_image(bytes, target_path)`](../../src-tauri/crates/app/src/commands/share.rs), shared with the Now Playing card) and writes via `spawn_blocking` — no `tauri-plugin-fs` dependency. The "copy" path stays in the browser and works on Chromium-based WebView (Edge on Windows, WKWebView on macOS); WebKitGTK on Linux historically refused image/png clipboard writes, so the error is surfaced rather than silently no-op'd.

## Now Playing share card

Same Save / Copy pattern as Wrapped, but applied to the **currently-playing track**. The Share button ([`ImmersiveShareButton`](../../src/components/player/ImmersiveShareButton.tsx)) in the [`ImmersiveView`](../../src/components/player/ImmersiveView.tsx) top bar generates a 1080×1080 square PNG via [`lib/nowPlayingCard.ts`](../../src/lib/nowPlayingCard.ts) — the cover artwork is drawn full-bleed under a dark wash for the background, then again as a centred 580 px tile with rounded corners + drop shadow, followed by title + artist + album text. The bottom of the card carries a thin accent strip in the artwork's dominant colour (sampled via the existing [`lib/dominantColor.ts`](../../src/lib/dominantColor.ts)) so each card visually nods to its source cover. Backend writes go through the same `save_share_image` Tauri command as Wrapped — the IPC channel is feature-agnostic so future share card flows (album, playlist) can reuse it without new commands. Disabled when no track is playing.

## Width & containers

Music browsing views (Home, Library, Playlist, Album, Artist, Liked, Recent, Statistics) render **full width** inside the center column — no `max-w-*` cap. The `p-8` gutter on the page scroller ([`AppLayout.tsx`](../../src/components/layout/AppLayout.tsx)) is the only horizontal breathing room. On a 2.5K display the table area gains ~800 px over the previous `max-w-6xl mx-auto` constraint.

Form-style views (Settings, About, Feedback) keep `max-w-4xl` because dense forms read better with a comfortable line length.

Track tables themselves are **borderless** — no `rounded-2xl border bg-white` card wrapper. The page already provides the visual frame; nesting another card just shrinks every row by ~80 px and breaks the Spotify-style "rows on the page" feel. The column-header `border-b` is the only separator between header and rows.

## Performance

- **Virtual scroll** — `@tanstack/react-virtual` on every long list (tracks, queue, playlist contents, statistics rows). Tables share the page-level scroller via [`usePageScroll()`](../../src/hooks/usePageScroll.ts) and compute `scrollMargin` from the parent's offset so the virtualiser knows where its content begins. Single Spotify-style scrollbar, no nested overflow.
- **Image cache** — in-memory LRU (`lib/imageCache.ts`) for `convertFileSrc` results so the same artwork URL isn't recomputed on every render.
- **Thumbnails** — 1× and 2× covers generated by [`thumbnails.rs`](../../src-tauri/crates/core/src/artwork/thumbnails.rs) with `fast_image_resize` (SIMD AVX/SSE/NEON depending on host) and served via the asset protocol.

## Player-bar layout

Right side of [`PlayerBar`](../../src/components/player/PlayerBar.tsx) is the highest-pressure real estate in the UI — every new feature wants an icon there. To keep the bar from running out of width on narrow windows, controls cluster by frequency:

| Tier         | Controls                                                                                                                                                                                                                                                                                            | Where                                                                                                                                                                                                                           |
| ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Primary**  | Lyrics, Queue, Device picker, "⋯", Volume, Mini-player, Immersive view, plus any pinned overflow item (A-B loop, Sleep timer, EQ presets). Every entry is opt-out from the user side via Settings → Playback (defaults match the pre-customisation layout — zero visible change after the upgrade). | Each button reads its visibility from `usePlayerBarLayout` ([`src/hooks/usePlayerBarLayout.ts`](../../src/hooks/usePlayerBarLayout.ts)). The same hook drives the live preview in the Settings panel.                           |
| **Overflow** | Playback speed (slider + presets), EQ presets, A-B loop, Sleep timer                                                                                                                                                                                                                                | [`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx) — "⋯" popover; trigger auto-hides when every overflow entry is pinned. EQ presets share their inner `EqPresetPanel` body with the primary popover variant. |
| **Pinnable** | A-B loop, Sleep timer, EQ presets (promote each to primary independently)                                                                                                                                                                                                                           | Settings → Playback → "Player bar layout" — single panel covering every button + the cover-click action.                                                                                                                        |

The Settings panel ([`PlayerBarLayoutCard`](../../src/components/views/settings/PlayerBarLayoutCard.tsx)) replaces the three earlier per-feature toggles (sleep timer / A-B loop / audio-quality footer). Layout is read through [`usePlayerBarLayout`](../../src/hooks/usePlayerBarLayout.ts) and writes are persisted via `setProfileSetting` + a single `waveflow:playerbar-layout-changed` window event so every consumer re-reads in one go (the legacy per-feature events `waveflow:sleep-timer-visibility` / `waveflow:ab-loop-visibility` / `waveflow:audio-quality-footer-visibility` are still observed by the hook for back-compat with any external dispatcher).

Order is **fixed** — drag-to-reorder would add a sortable library dependency and a fairly minor UX gain since the player bar is small and the conventional left-to-right sequence (overflow → utility toggles → volume → window-management) is what users from Spotify / Apple Music already expect.

The **cover thumbnail** at the bottom-left of the player bar carries its own action (`ui.cover_action`, default `immersive`):

| Value         | Behaviour                                                                                       |
| ------------- | ----------------------------------------------------------------------------------------------- |
| `immersive`   | Open the immersive view — classic by default; merged columns + native fullscreen opt-in         |
| `now_playing` | Toggle the right-edge Now Playing panel — Spotify-style "click cover, see lyrics + cover wall". |
| `none`        | No-op. Useful for users who keep mis-triggering it.                                             |

When adding a new player-bar action: default it into the overflow menu first — promote to primary only when usage data or user feedback warrants it. Always wire it through `PLAYER_BAR_LAYOUT_KEYS` + the `PlayerBarLayoutCard` toggle grid so users can opt out from one place. The "⋯" trigger auto-hides when its menu would be empty.

**Volume control** ([`VolumeControl`](../../src/components/player/VolumeControl.tsx)) supports three input modalities: pointer drag on the track, keyboard arrows / Home / End when the slider has focus (5 % step, 0 / 100 jumps), and **mouse-wheel scroll** anywhere over the icon + track area (5 % step, wheel up raises). The wheel handler is bound through `addEventListener('wheel', ..., { passive: false })` so it can `preventDefault` the underlying page scroll — React 17+'s JSX `onWheel` is passive and would let the track list behind the player bar scroll at the same time. Horizontal-only scrolls (`deltaY === 0`, e.g. trackpad sideways swipes) are ignored so they don't get treated as a volume-down tick.

**Playback speed** lives inside [`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx) (range slider + five presets) rather than a dedicated bar button — it's used too rarely to deserve a permanent slot. When speed ≠ 1×, the "⋯" trigger surfaces a compact `1.25×` badge in emerald (same corner as the sleep-timer countdown — the countdown wins when both are active). See [playback / Playback speed](playback.md#playback-speed-05--2) for the backend side.

### Pin toggles

A-B loop and Sleep timer are **always available** — they live in the "⋯" overflow menu by default. The pin toggles let frequent users promote them to a primary slot on the bar so they're one click away. Both default to **off**:

| Setting key           | Pinned button rendered in primary slot | Default |
| --------------------- | -------------------------------------- | ------- |
| `ui.show_sleep_timer` | Moon icon (sleep timer menu)           | off     |
| `ui.show_ab_loop`     | Repeat icon (A-B loop)                 | off     |

When a pin is OFF, the entry stays in the overflow menu and the sleep-timer countdown badge surfaces on the "⋯" trigger itself so the user keeps live feedback while the timer is armed. The PlayerBar listens to `waveflow:sleep-timer-visibility` / `waveflow:ab-loop-visibility` window events dispatched by the Settings toggle so the layout re-renders without a polling loop.

The overflow popover itself is capped at `max-h-[calc(100dvh-7rem)]` with `overflow-y-auto overscroll-contain` ([`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx)). On a 1080p display with nothing pinned, the stack (speed slider + 5-preset grid, EQ section, A-B loop row, sleep-timer 6-preset grid + end-of-track + custom-minutes form) would otherwise run past the viewport top. `100dvh` rather than `100vh` keeps the math right when Tauri window chrome hides on Linux/macOS fullscreen; `overscroll-contain` prevents wheel/touch scrolls from chaining to the page underneath.

### Audio-quality footer + pipeline popover

The opt-in audio-quality footer ([`AudioQualityFooter`](../../src/components/player/AudioQualityFooter.tsx), pinned via Settings → Appearance → Player bar layout) is a thin strip below the player bar that surfaces the source file specs in compact form (`48 kHz · 256 kb/s · 6 Mo` on the left, `AAC · 24bit · 48kHz` on the right; bitrates ≥ 1000 kbps render as `Mb/s`). When the engine is resampling — source rate ≠ output device rate — the left chunk renders an arrow instead: `48 kHz → 44.1 kHz · …`, so the user can spot the conversion at a glance without opening the popover. The arrow is gated on the device rate being known (the engine reports `0` before the first stream opens); otherwise we fall back to the source rate alone rather than printing a misleading `48 kHz → null`. The Hi-Res pill surfaces when [`isHiRes`](../../src/lib/hiRes.ts) accepts the source bit depth / sample rate combination.

### Hi-Res / DSD badge

[`HiResBadge`](../../src/components/common/HiResBadge.tsx) is the green pill that decorates track rows, album grid tiles, and the player-bar metadata when the source qualifies as Hi-Res (`isHiRes` — ≥ 24-bit, ≥ 44.1 kHz) or as DSD (`dsdLabel` returns `DSD64` / `DSD128` / …). Three variants:

| Variant   | Used in                               | Style                                                                                           |
| --------- | ------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `overlay` | Album / artist grid covers (default). | Absolute-positioned pill in the cover's top-left corner with a drop shadow.                     |
| `inline`  | TrackTable rows, sidebar lists.       | Inline rounded pill next to the title.                                                          |
| `text`    | Player bar — under the artist name.   | Spotify-style minimal green uppercase text, no pill background, blends into the metadata stack. |

All variants are gated by [`useHiResBadgeVisibility`](../../src/hooks/useHiResBadgeVisibility.ts), which reads `profile_setting['ui.show_hi_res_badge']` (default `true`) and re-reads on the `waveflow:hi-res-badge-visibility` window event. Settings → Appearance ships [`HiResBadgeCard`](../../src/components/views/settings/HiResBadgeCard.tsx) to flip the flag — when off, every mounted `HiResBadge` returns `null` in one render, including the player-bar text label. Per-profile so a kid's profile can hide the audiophile chrome while the audiophile profile keeps it.

Hovering (or keyboard-focusing) the footer opens [`AudioPipelinePopover`](../../src/components/player/AudioPipelinePopover.tsx) — an audiophile-grade breakdown of what the engine is actually doing.

#### Sections displayed

- **Source** — codec, sample rate, bit depth, bitrate, channel layout (`Mono` / `Stereo` / `3.0` / `4.0` / `5.0` / `5.1` / `6.1` / `7.1`).
- **Processing** — chips lighting up for every active stage. The two conversion chips inline the actual delta so they match the footer's arrow notation: `Rééchantillonnage 48 → 44.1 kHz`, `Downmix 5.1 → Stereo`. The other chips stay as bare labels: `DSD → PCM`, `EQ`, `ReplayGain`, `Normalize`, `Mono` mixdown, `Speed ≠ 1×`. No chip → "Aucun traitement appliqué".
- **Output** — device sample rate + channel layout read from the live engine snapshot (`PlayerStateSnapshot.sample_rate` / `channels`), not the track row, so resampling and downmix are reflected correctly.

#### Bit-perfect conditions

The green `Bit-perfect` pill at the bottom appears **only** when no processing chip is active **and** the source rate matches the output rate. Any single chip lit (including `EQ` and `Speed`) suppresses it.

#### State refresh

Hydration on open runs `playerGetState` / `playerGetAudioSettings` / `playerGetEq` in parallel so every read reflects the freshest engine state — the EQ may have been flipped from another popover seconds ago and we want truth, not stale React state. The popover unmounts on hover-leave so we never display stale data once it closes. 120 ms open / 200 ms close hover delays so brushing the footer doesn't flicker the popover open.

## Keyboard shortcuts

Action ↔ key bindings live in [`src/lib/shortcuts.ts`](../../src/lib/shortcuts.ts) (12 actions, defaults like `Space` → play/pause, `←`/`→` → previous/next, `M` → mute, `S` → shuffle, `R` → repeat, `L` → toggle lyrics, `Shift+L` → like). [`useGlobalShortcuts`](../../src/hooks/useGlobalShortcuts.ts) is mounted once in [`AppLayout`](../../src/components/layout/AppLayout.tsx) and attaches a single `window.keydown` listener that dispatches against `PlayerContext`. Listener skips when the focus target is `INPUT` / `TEXTAREA` / `contenteditable` so typing in a search box doesn't toggle shuffle.

User overrides are stored per-profile in `profile_setting['ui.shortcuts']` as a JSON object containing only customised actions — defaults stay implicit, so future default tweaks land for any binding the user hasn't touched. Settings → Raccourcis clavier ([`ShortcutsCard`](../../src/components/views/settings/ShortcutsCard.tsx)) captures keys in capture-phase so the rebind UI doesn't fire the global handler. Conflicts auto-resolve by stealing the combo from whoever previously owned it. AboutView reads the same setting and re-renders on the `waveflow:shortcuts-changed` window event.

## Theming & motion

- **Dark mode** — animated radial transition via the [View Transitions API](https://developer.mozilla.org/en-US/docs/Web/API/View_Transitions_API). Falls back to an instant swap when unsupported.
- **`prefers-reduced-motion`** respected for the radial transition and for animated SVGs.
- **Single-click play** — optional Settings toggle; the default is double-click to mirror Apple Music / Finder.
- **Framer Motion** — `motion/react` provides micro-interactions (sidebar nav reorder, modal open, view fade-in, queue drag). One global [`SkinMotionWrapper`](../../src/components/layout/SkinMotionWrapper.tsx) feeds skin-specific `transition` config to the `MotionConfig` provider so per-skin springs (Pulse uses `cubic-bezier(0.34, 1.56, 0.64, 1)`, Lounge stays tame, etc.) apply automatically without touching call sites.

## Skins

Skins are an **orthogonal axis to the 14 colour themes**: a skin re-skins surfaces, typography, motion and signature elements (e.g. Editorial's drop caps, Pulse's vinyl-spin cover); the theme picks the OKLCH accent. Every skin × theme combination is valid → **5 × 14 = 70 visual identities**.

<!-- markdownlint-disable MD060 -->

| Skin       | Direction                            | Signature                                                                                                                              |
| ---------- | ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- |
| `studio`   | Apple Music baseline                 | Inter / system-ui, soft shadows, rounded-12 — the calm default                                                                         |
| `editorial`| Broadsheet "WaveFlow Gazette"        | Playfair Display 900 + Lora body, `<EditorialMasthead>` with locale-aware `Intl.DateTimeFormat`, `::first-letter` drop caps, halftone N&B covers via `mix-blend-luminosity`, ruler-tick progress bar via `repeating-linear-gradient`, `counter-reset` page numbering ("P. 1", "P. 2") |
| `lounge`   | "Listening Room" warm-burgundy glass | Inter, [`SkinAmbientBackdrop`](../../src/components/layout/SkinAmbientBackdrop.tsx) blurs the cover (`filter: blur(140px) saturate(280%)`) into the body, white-overlay tint over the cover dominates the chrome |
| `pulse`    | OLED "club control panel"            | Space Grotesk + Space Mono, dual-neon magenta/cyan aurora blobs, `///` mono pills, vinyl-spin cover via [`SkinPlayingState`](../../src/components/layout/SkinPlayingState.tsx) mirroring `usePlayer().isPlaying` to `[data-is-playing]`, floating pill PlayerBar |
| `liquid`   | Apple Vibrancy material              | DM Sans Variable (`opsz` axis 9..40), 8-layer inset `box-shadow` recipe `--liquid-glass` for "real glass" surfaces, aurora drift `body::before` (28 s), theme-aware light/dark token swap via `:not(.dark)`, `--liq-action` cyan/blue swap for contrast |

<!-- markdownlint-enable MD060 -->

**Architecture** :

- Token system + `SkinId` union live in [`src/lib/skins.ts`](../../src/lib/skins.ts); `applySkin()` writes `data-skin` on `<html>`.
- Per-skin overrides live in `src/styles/skins/{editorial,lounge,pulse,liquid}.css` (Studio = no overrides, baseline). The four files are imported from `src/app.css`.
- [`src/app.css`](../../src/app.css) extends the Tailwind `dark` variant via `@custom-variant dark (.dark, .dark *, :root[data-skin="lounge"] *, :root[data-skin="pulse"] *)` — Lounge/Pulse fire `dark:*` utilities automatically because they're always "dark" by design (Liquid stays theme-aware).
- **Local-first typography** — all skin fonts are bundled via `@fontsource` / `@fontsource-variable` (Playfair Display, Lora, Space Grotesk, Space Mono, DM Sans Variable) and imported at the top of [`src/main.tsx`](../../src/main.tsx). Zero network at runtime — no Google Fonts request.
- **Motion** — each skin declares its own `MotionConfig` spring in `SkinMotionWrapper`. Skins with strong identity (Pulse, Editorial) override the spring; calmer skins (Studio, Lounge, Liquid) inherit the soft default.

The picker lives in Settings → Appearance via [`SkinPickerCard`](../../src/components/views/settings/SkinPickerCard.tsx), alongside the theme picker and `PlayerBarLayoutCard`. View-Transitions API also drives skin swaps (radial reveal on click), with the same try/catch fallback used by the theme picker for WebKitGTK builds.

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

- **Create** → "+" tile in the select view → name + colour picker → backend [`create_profile`](../../src-tauri/crates/app/src/commands/profile.rs) reserves the row, materialises `profiles/<id>/`, and runs the initial migration. The freshly-created profile is auto-activated.
- **Delete** → Netflix-style "Manage" toggle (pencil ↔ check) in the top-left corner reveals a red trash badge on every non-active profile; tapping it opens a destructive confirmation view. Backend [`delete_profile`](../../src-tauri/crates/app/src/commands/profile.rs) refuses the active profile and the last remaining profile. The guard is **atomic**: a single SQL statement (`DELETE FROM profile WHERE id = ? AND (SELECT COUNT(*) FROM profile) > 1`) couples the "must not be last" predicate with the mutation so two concurrent deletes can never empty the table. Disambiguation between "not found" and "last profile" is handled on the failure path. After the row is removed, `profiles/<id>/` is wiped from disk and `app.last_profile_id` is cleared if it pointed to the deleted profile.

### Export / import (`.waveflow` archive)

[`commands/profile_io.rs`](../../src-tauri/crates/app/src/commands/profile_io.rs) packages a profile into a single `.waveflow` (zip) file containing `manifest.json` + `data.db` + the per-profile `artwork/` directory. Settings → Stockage exposes both buttons.

- **Export:** the active-profile path runs `PRAGMA wal_checkpoint(TRUNCATE)` first so the bundled DB captures every committed page (otherwise a busy WAL would leave the archive holding a partial snapshot). The CPU-bound zip work runs on `tokio::task::spawn_blocking`.
- **Import:** always allocates a fresh profile row — never overwrites — then extracts the archive under `profiles/<new_id>/`. Failures roll the row back so a half-imported profile doesn't survive the error. Before the sqlx migrator runs, [`normalise_migration_checksums`](../../src-tauri/crates/app/src/commands/profile_io.rs) rewrites `_sqlx_migrations.checksum` for every version present in both the archive and the local migrator — older builds checked out migration files with CRLF endings (Windows `core.autocrlf=true` + no `.gitattributes` lock) so their stored SHA-384 differs from the same SQL re-hashed today, even though the DDL is identical. A `.gitattributes` at repo root now pins `*.sql` / `*.rs` / `*.ts` / etc. to LF so future archives stay byte-stable. Once normalised, the new pool is opened once so any pending sqlx migrations replay before the user switches to it. An archive whose `_sqlx_migrations` lists a version unknown to the local migrator is rejected — that means the export came from a newer build.
- **Out of scope:** the shared `app.db` (Last.fm key, Discord opt-in, `network.offline_mode`) belongs to the install, not the profile. The shared `metadata_artwork/` cache (Deezer pictures, etc.) is re-fetchable so we skip it to keep archives small.
- **Manifest:** `archive_version` (currently `1`) gates compatibility — a future schema-incompatible bump refuses imports rather than silently corrupting the new profile. `app_version` and the source profile name / id are recorded for diagnostics.

### Auto-backup

Opt-in scheduled mirror of the manual export so the user's playlists / likes / ratings / history survive a SQLite corruption or disk failure. Implementation in [`backup.rs`](../../src-tauri/crates/app/src/backup.rs):

- **Config** lives in `app_setting` (install-wide, not per-profile): `backup.enabled` (bool, default OFF), `backup.interval_days` (1-90, default 7), `backup.folder` (string; empty = default `<app_data>/waveflow/backups/`), `backup.retention` (1-50, default 5 — per profile), `backup.last_run_at` (epoch ms).
- **Loop** is a single tokio task started once at boot ([`spawn_backup_loop`](../../src-tauri/crates/app/src/backup.rs)). When disabled, parks on a `tokio::sync::Notify` (zero cost) until the user toggles. When enabled, computes the next deadline as `last_run_at + interval_days * 86_400_000` and uses `tokio::select!` between a sleep and the same `Notify` so config changes wake it without waiting for the old sleep to expire.
- **Pass** ([`run_one_backup`](../../src-tauri/crates/app/src/backup.rs)) iterates every row in `profile`, calls the shared [`profile_io::write_archive`](../../src-tauri/crates/app/src/commands/profile_io.rs) (pub-crate-ified from the manual-export path so the two stay bit-compatible), and applies retention per profile (`<sanitized-name>-*.waveflow` sorted by mtime, oldest beyond `retention` deleted). The active profile gets a `PRAGMA wal_checkpoint(TRUNCATE)` first; inactive profiles are already cold on disk (the pool ran a checkpoint at switch / shutdown).
- **Failure isolation:** per-profile errors are logged but don't abort the pass — one corrupt profile shouldn't block backups of the healthy ones.
- **Commands** in [`commands/backup.rs`](../../src-tauri/crates/app/src/commands/backup.rs): `get_backup_config`, `set_backup_config` (also signals the loop), `run_backup_now`. UI is [`BackupCard`](../../src/components/views/settings/BackupCard.tsx) in Settings → Stockage right after the manual export/import.

## Settings categories

[`SettingsView`](../../src/components/views/SettingsView.tsx) is split into seven Lokal-style horizontal tabs rendered as a proper ARIA `role="tablist"` at the top of the page (keyboard-navigable, `aria-selected` per panel):

| Tab            | Houses                                                                                                  |
| -------------- | ------------------------------------------------------------------------------------------------------- |
| `library`      | Library folders, scan-on-start, file watcher                                                            |
| `playback`     | EQ, crossfade, ReplayGain, normalisation, WASAPI exclusive, mono                                        |
| `integrations` | Last.fm, Discord RPC, Deezer enrichment, DLNA media server                                              |
| `appearance`   | Theme picker (14 presets) + player-bar layout                                                           |
| `data`         | Profile export / import, auto-backup, statistics export, offline                                        |
| `shortcuts`    | Per-action keyboard rebinder ([`ShortcutsCard`](../../src/components/views/settings/ShortcutsCard.tsx)) |
| `diagnostics`  | Log folder reveal, recent log tail, app info                                                            |

Only one panel mounts at a time, so heavy sub-views (EQ visualiser, backup card, shortcuts editor) don't run their effects until the user opens that tab.

## Theme system

[`THEME_PRESETS`](../../src/lib/themes.ts) ships 14 presets split into two visual rows:

| Row   | Presets                                                                 |
| ----- | ----------------------------------------------------------------------- |
| Light | Émeraude · Midnight · Sunset · Lavender · Crimson · Ocean               |
| Dark  | Émeraude · OLED · Midnight · Sunset · Lavender · Crimson · Ocean · Neon |

Each preset declares a 50→950 OKLCH accent palette + a `mode` (`light` / `dark`) + an `ambient` body color + optional `surfaceDark` / `surfaceDarkElevated` overrides. `applyTheme` writes `--accent-50..950`, `--ambient-bg`, `--color-surface-dark`, `--color-surface-dark-elevated` on `<html>`, and Tailwind v4's `@theme inline` block in [`app.css`](../../src/app.css) remaps every `bg-emerald-*` / `text-emerald-*` utility + the `bg-surface-dark*` utilities to those vars — so a swap re-tints the entire app without touching a single component.

The `surfaceDark` family is **non-optional in practice** for any themed dark preset: leaving it on the default `#121212` produces a flat charcoal sidebar against a violet / amber / rose body (the bug fixed when these tokens went theme-aware). Each themed dark preset sets `surfaceDark = ambient` and `surfaceDarkElevated ≈ ambient + small lightening step` so sidebar / right panels / player bar all carry the theme tint while elevated cards still read above the body.

A small inline script in [`index.html`](../../index.html) runs **before React mounts** to paint the right `dark` class + `data-theme` + `--ambient-bg` from the stored preset id, so a fresh boot doesn't flash white when the default theme is dark. The script keeps a `LIGHT_IDS` lookup mirroring `themes.ts` — both tables must stay in sync if a preset is added or removed.

Switching uses [View Transitions API](https://developer.mozilla.org/en-US/docs/Web/API/View_Transitions_API): a radial reveal from the click point on supported browsers, a plain crossfade on the rest. [`setThemeId`](../../src/contexts/ThemeContext.tsx) wraps `document.startViewTransition` in try/catch because some WebKitGTK builds throw synchronously — the fallback calls `setTheme(next)` directly so the persisted id never desyncs from the applied palette. The persisted id lives in `localStorage['waveflow.theme.id']`; the legacy `waveflow.theme.is_dark` boolean from v1.x is migrated on first read (written under the new key + removed) so a downgrade-then-upgrade cycle can't silently overwrite a custom preset.

## Onboarding

[`OnboardingModal`](../../src/components/common/OnboardingModal.tsx) walks new profiles through a Lokal-style multi-step wizard. Steps in order:

1. **welcome** — branding + privacy pitch.
2. **language** — picker over [`SUPPORTED_LANGUAGES`](../../src/i18n/index.ts); persists immediately so the rest of the wizard renders in the chosen locale.
3. **profile** _(conditional)_ — name the auto-created "Default" profile in place via [`rename_profile`](../../src-tauri/crates/app/src/commands/profile.rs). Safe against the active profile since only `app.db` is touched; the per-profile pool keeps its open handle. Skipping the rename (input unchanged) avoids the backend round-trip entirely. The step is **omitted entirely** when the active profile's name isn't the literal `"Default"` — i.e. profiles created through the New Profile modal already carry a user-supplied name, so the rename step would just ask the same question twice. `"Default"` is the hardcoded auto-bootstrap name from [`state.rs::create_default_profile`](../../src-tauri/crates/app/src/state.rs) (not localised, so the comparison is reliable).
4. **localOnly** — explainer that the library never leaves the device unless the user opts into Last.fm / Discord later.
5. **folder** — calls [`pickFolder`](../../src/lib/tauri/dialog.ts) to select a music root and creates the first library entry.
6. **lastfm** — optional Last.fm API key + secret pairing (skippable). Status lives in [`integration.rs`](../../src-tauri/crates/app/src/commands/integration.rs).
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

Tauri updater plugin with a signed update flow. The update banner offers "Install now" without forcing a relaunch interruption. **Wired in release builds only** — in `tauri dev` the local source tree wouldn't have a signed manifest to fetch, so the plugin would just spam errors. See [`lib.rs`](../../src-tauri/crates/app/src/lib.rs) for the `#[cfg(not(debug_assertions))]` gate.
