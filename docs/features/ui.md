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

## Settings navigation

Settings uses a persistent category list on wide containers and a compact
category selector when the available content width is below 56rem. The layout
responds to its container, including space taken by the application's sidebar.
Only the selected category is mounted. Existing category IDs remain valid for
deep links and the remembered selection; a fresh visit defaults to General.

The ten categories are General, Library, Playback and audio, Appearance,
Images and lyrics, Connections, Extensions, Keyboard shortcuts, Storage and
backups, and Maintenance. Library owns scanning, duplicates and music analysis;
Storage and backups owns profile exports, caches and artwork maintenance.
Images and lyrics brings artwork retrieval and lyrics preferences together.

[`settingsCatalog.ts`](../../src/components/views/settings/settingsCatalog.ts)
indexes translated category, group and setting labels for local settings search.
A result opens its category, expands an advanced group when necessary, and moves
focus to that group. Keep the search keys and group IDs aligned when adding or
moving a setting. New navigation strings live under `settings.organization` in
all 17 locales.

[`SettingsGroup`](../../src/components/views/settings/SettingsGroup.tsx) supplies
section headings and native disclosure controls for advanced audio, network
sharing, artwork cleanup and application reset. Scoped settings row styles live
in `settings/settings.css`; existing cards retain their own behavior and controls.
A setting's secondary line (subtitle or input hint) carries the
`settings-description` class rather than colour utilities: the stylesheet owns its
grey, line height and 65ch measure, and its dark selector mirrors the `dark:`
variant so the Lounge and Pulse skins get the dark shade on a light theme. Rows
wrap their control under the label when the content column (a named
`settings-content` container, not the viewport) is narrower than 40rem.
[`SettingsCategoryHeader`](../../src/components/views/settings/SettingsCategoryHeader.tsx)
renders each category's title and description from its catalog entry.

## Panels

- [`NowPlayingPanel`](../../src/components/layout/NowPlayingPanel.tsx) — large artwork, clickable artists, "About the artist" section populated from the Deezer + Last.fm caches, and a "Next in queue" teaser with an "Open queue" link that hands the right slot off to `QueuePanel`. Lightbox on cover click.
- [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) — current queue with drag reorder, jump-to-track, clear queue. Right-clicking a queue row opens the same track context menu the list views have (Show in Explorer, Properties, play-next / add-to-queue, like, rating…) via [`usePlayerTrackContextMenu`](../../src/hooks/usePlayerTrackContextMenu.tsx) — the player-surface wrapper that supplies the liked-ids lookup + create-playlist modal the base [`useTrackContextMenu`](../../src/hooks/useTrackContextMenu.tsx) needs. Radio/stream rows (negative sentinel id, no local file) are skipped. Payload→`Track` widening is the shared [`queuePayloadToTrack`](../../src/lib/queueTrack.ts). Same menu is reachable by right-clicking the title in [`ImmersiveNowPlaying`](../../src/components/player/ImmersiveNowPlaying.tsx) (library tracks only). Reporter request.

### Opening the track menu from the keyboard

Right-click has no keyboard equivalent unless one is wired by hand, so every surface that shows a track menu answers the same two keys on the focused row — **Menu** (the application key) and **Shift+F10**, its stand-in on keyboards without it. The detection and the anchor live in [`contextMenuKeys.ts`](../../src/lib/contextMenuKeys.ts) and are surfaced as `openFromKeyboard` on [`useTrackContextMenu`](../../src/hooks/useTrackContextMenu.tsx) (forwarded by the player wrapper), rather than reimplemented per view — issue #436 exists precisely because fixing one surface would have produced the inconsistency it set out to remove.

`openFromKeyboard` returns `true` when it opened a menu, so a row's existing `onKeyDown` early-returns instead of re-deriving the condition, and Enter / Space keep playing the track. A keyboard-opened menu anchors just inside the row's bottom-left corner (no pointer to place it at); `ContextMenu` still flips it when it would overflow the viewport.

Once open, [`ContextMenu`](../../src/components/common/ContextMenu.tsx) focuses its first item, moves with Up/Down/Home/End (wrapping), closes on Escape **and Tab** — a menu is not a tab stop, and letting Tab escape behind an open menu is worse than dismissing it — and **restores focus to the element that opened it** on unmount, guarded on `isConnected` for the case where the action just taken removed that row.

Two surfaces needed more than a key handler: history rows had no `tabIndex` at all (unreachable by keyboard, menu or not) and are now focusable with Enter/Space playing the row, and the immersive title takes `tabIndex` + `aria-haspopup="menu"` only while a menu is available — deliberately not `role="button"`, since it opens a menu but remains the heading.

- [`LyricsPanel`](../../src/components/layout/LyricsPanel.tsx) — synced or static lyrics with auto-scroll.
- [`NowPlayingChevronTab`](../../src/components/layout/NowPlayingChevronTab.tsx) — right-edge floating tab visible only when no panel is open.

## Long-running tasks

One status bar for every operation that takes long enough to wonder about
(issue #601): the library scan, the analysis sweep, lyrics prefetch, the
catalogue mirror, reconciliation, uploads, backups, the thumbnail pass.
[`TaskStatusBar`](../../src/components/layout/TaskStatusBar.tsx) subscribes to
`tasks:changed` before asking for a snapshot, so a task that starts between the
two is not missed.

The registry ([`tasks.rs`](../../src-tauri/crates/app/src/tasks.rs)) deliberately
does **not** implement cancellation. Five of these operations already had a
stopping point of their own, each in a different place, so a task registers how
it stops and the registry routes to it — re-deriving five stopping points would
have got at least one wrong. A task whose whole mechanism is a flag registers
`Cancellation::Flag` and polls `is_cancelling()`.

Two things a stopping point has to respect, both learned the hard way:

- **A scan's safe stopping point is not between two files.** The scan ends with
  a pass that marks everything the walk did not reach as `is_available = 0`.
  Stopping halfway and letting that run marks a working library unavailable
  *because the user pressed stop*. The sweep is skipped entirely on a cancelled
  scan, and the one-off ReplayGain and custom-tag backfill markers are not
  written either — they never run twice, so recording them would leave those
  tracks without their data permanently.
- **A cancellation callback is bound to the run that registered it.** The
  stopping flags are process-wide statics shared by every run of their
  operation, so a callback picked up as its task retires could otherwise land on
  the next run and stop an analysis nobody asked to stop.

Progress is throttled to 250 ms, except the final tick, so a task that ends on a
full bar shows a full bar.

## Immersive view

[`ImmersiveView`](../../src/components/player/ImmersiveView.tsx) (`fixed inset-0 z-100`) is the Apple-Music-TV-style fullscreen view that turns the current track into the focal point: the cover hero + metadata + transport centred as one group on the left, and a **tabbed control panel on the right** — Lyrics / Queue (issue #328). Background is a blurred copy of the artwork (with a 65% black wash) so the view stays visually anchored without extra theming.

It merges what used to be two mutually-exclusive overlays (`FullscreenNowPlaying` + `FullscreenLyrics`) into one view, decomposed as: [`ImmersiveNowPlaying`](../../src/components/player/ImmersiveNowPlaying.tsx) (left column — cover/metadata/transport), [`ImmersiveSidePanel`](../../src/components/player/ImmersiveSidePanel.tsx) (right control panel — segmented Lyrics / Queue tabs) wrapping [`ImmersiveLyricsColumn`](../../src/components/player/ImmersiveLyricsColumn.tsx) (karaoke scroller) + [`ImmersiveQueueTab`](../../src/components/player/ImmersiveQueueTab.tsx) (now-playing + up-next, double-click to jump; reuses `player_get_queue` / `player_jump_to_index`, white-on-dark styled — the docked [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) keeps the theme-aware styling + drag-reorder), and [`ImmersiveShareButton`](../../src/components/player/ImmersiveShareButton.tsx) (share-as-PNG in the top bar). The orchestrator owns the shared close / share / panel-toggle chrome.

**One lyrics fetch, three consumers.** The lyrics state (three-tier fetch + parse + active-line / active-word tracking + import/refetch/clear, with every mid-flight staleness guard) lives in [`useTrackLyrics`](../../src/hooks/useTrackLyrics.ts). The side [`LyricsPanel`](../../src/components/layout/LyricsPanel.tsx), `ImmersiveView` and the [mini-player's lyrics overlay](#mini-player) all consume it, so the merged view never threads state from the side panel (the old `FullscreenLyrics` did, via a `createPortal` from `LyricsPanel`) — opening the immersive view no longer force-opens the right-edge panel. Every hook instance keys on the live `currentTrack`, so surfaces open at once resolve the later `fetch_lyrics` calls from the backend cache. The mini-player's copy lives in a second webview, so its instance is a genuinely separate fetch rather than a second consumer of the same one — which is why that overlay unmounts when closed.

**Two genuinely distinct interfaces** ([`useImmersivePrefs`](../../src/hooks/useImmersivePrefs.ts), per-profile; `merged_lyrics` + `use_native_fullscreen` both default **OFF** so the view is exactly the pre-#328 experience out of the box — each is opt-in) — the merged layout is an _option_, not a replacement:

- `immersive.merged_lyrics` ON + window ≥ 900 px → **merged**: now-playing left (~60%) + tabbed control panel right (~40%). The top-bar `PanelRight` toggle hides/shows the panel (left re-centres when hidden); the active tab (Lyrics / Queue) persists across hide/show. A clean lyrics miss keeps a discreet "No lyrics — Import / Fetch" CTA so the per-track relayout stays calm.
- pref OFF **or** window < 900 px → **classic**: now-playing fullscreen with a Mic2 button that flips to a lyrics-only fullscreen — the original behaviour, left intact (no control panel, no queue tab). The entry point (cover/now-playing vs lyrics button) picks the first side.

**Native fullscreen** (`immersive.use_native_fullscreen`, opt-in, default OFF) drives the OS window into real fullscreen on open via `getCurrentWindow().setFullscreen(true)`, capturing the prior maximized state in a ref and restoring it on close — so the OS chrome (title bar, taskbar) actually disappears. This needs the `core:window:allow-set-fullscreen` / `allow-is-fullscreen` / `allow-is-maximized` / `allow-maximize` permissions in [`capabilities/default.json`](../../src-tauri/crates/app/capabilities/default.json) (added with the feature — capabilities are compiled into the binary, so adding them requires a Rust rebuild). OFF keeps the in-window overlay for multi-monitor users who want the rest of their desktop visible. Escape (or the X) closes the view; exiting OS fullscreen another way (F11) is deliberately **decoupled** — it leaves the view as a windowed overlay rather than dismissing it. Both toggles live in [`ImmersiveViewCard`](../../src/components/views/settings/ImmersiveViewCard.tsx) under Settings → Appearance → Immersive view, and the feature is surfaced as a discreet tip on the onboarding "done" step.

Entry points in the [`PlayerBar`](../../src/components/player/PlayerBar.tsx): clicking the cover (mirrors Spotify) or the Immersive button, plus the Maximize2 icon in the side `LyricsPanel` header. `PlayerContext` exposes `immersiveOpen` + `immersiveInitialTab` + `openImmersive`/`closeImmersive`; the old `openFullscreenNowPlaying`/`openFullscreenLyrics`/`close*` names stay as back-compat aliases that all drive the one merged view.

**Transition hygiene** — the view paints a solid `bg-zinc-950` on the outer wrapper from the first frame; the `animate-fade-in` keyframe lives on the inner backdrop + foreground layers, not the wrapper. Without that opaque base the wrapper's own opacity ramp (0 → 1 over 300 ms) would let the page underneath bleed through during the transition.

**Skin neutrality** — the immersive view carries `role="dialog"` + a `shadow-2xl` cover, which the skins' modal / surface / text-colour chrome would otherwise capture (Liquid-light + Editorial repainted it as a light glass slab with dark, invisible text + washed-out controls; Lounge / Pulse flattened the cover backdrop). The root is tagged `data-immersive` + a nested `dark` context (so the shared `PlaybackControls` / `VolumeControl` / `ProgressBar` render their dark-theme variants over the always-dark backdrop), and every skin's relevant rules carry `:not([data-immersive]):not([data-immersive] *)` so the view renders identically across skins.

**Long titles scroll** — the now-playing title and the bottom `PlayerBar` title use [`MarqueeText`](../../src/components/common/MarqueeText.tsx): it measures overflow via a `ResizeObserver` and, only when the text actually overflows, glides it end-to-end and back (ping-pong with a pause at each extremity, `prefers-reduced-motion` respected) instead of truncating. Toggleable per-profile via [`ScrollTitlesCard`](../../src/components/views/settings/ScrollTitlesCard.tsx) under Settings → Appearance → Player and window ([`useScrollLongTitles`](../../src/hooks/useScrollLongTitles.ts), `ui.scroll_long_titles`, default ON — off falls back to ellipsis truncation).

## Track Canvas

[`CanvasStage`](../../src/components/player/CanvasStage.tsx) plays a short muted looping mp4 **behind the now-playing view**, Spotify-Canvas-style (issue #442) — the clip cleanly replaces the static cover (`object-cover` fill, no blurred backdrop) in [`ImmersiveNowPlaying`](../../src/components/player/ImmersiveNowPlaying.tsx) + [`NowPlayingPanel`](../../src/components/layout/NowPlayingPanel.tsx). It is keyed **per track** (matching Spotify — the clip belongs to the current song, unlike the per-album [motion artwork](plugins.md#apple-motion-artwork-metadata-world)) and takes precedence over the plugin motion cover when both exist (**Canvas > motion cover > static cover**).

**Sourcing — v1 is manual mp4 only** (core-only, no plugin): the user picks a local `.mp4` via the immersive top-bar "⋯" → [`CanvasPickerModal`](../../src/components/common/CanvasPickerModal.tsx) (set / remove). The file is validated by mp4 magic bytes (`ftyp`), size-capped (64 MiB), BLAKE3-hashed and **published atomically** — staged to a unique temp then `hard_link`-ed onto the target (no-replace; a concurrent importer of the same hash loses harmlessly) — into the never-evicted per-profile `canvas/` dir ([`AppPaths::profile_canvas_dir`](../../src-tauri/crates/app/src/paths.rs)), via the shared [`media_file::store_hash_addressed_mp4`](../../src-tauri/crates/app/src/commands/media_file.rs) helper the manual motion cover (#408) also uses. The row in `track_canvas` (per-profile, `ON DELETE CASCADE`) is itself the "has a manual Canvas" signal. Commands: [`commands/canvas.rs`](../../src-tauri/crates/app/src/commands/canvas.rs) (`get_track_canvas` / `set_track_canvas_from_file` / `clear_track_canvas`).

**Sourcing — plugin (issue #473).** When no manual clip is set, [`useTrackCanvas`](../../src/hooks/useTrackCanvas.ts) falls back to enabled [`canvas`-world plugins](plugins.md#the-canvas-world-waveflowcanvasv1) via [`fetch_track_canvas`](../../src-tauri/crates/app/src/commands/canvas.rs), which returns a **remote** mp4 URL — [`CanvasStage`](../../src/components/player/CanvasStage.tsx) loads a local path through `convertFileSrc` and a remote URL directly (the `http(s)` prefix tells them apart). The plugin clip sits one rung below the manual one: **manual Canvas > plugin Canvas > motion cover > slideshow > static cover**. The fanout is **fail-soft** (a plugin error/panic/timeout is logged + skipped, never breaks playback), and the picker's "remove" affordance keys on the manual clip only (a plugin URL doesn't read as "a Canvas to remove"). By default the webview streams the plugin's remote mp4; an **opt-in local cache** (`app_setting['canvas.cache_enabled']`, default OFF, toggle in **Settings → Storage and backups → Folders and caches** beside the cache-location card) downloads it into an app-wide LRU (`canvas_cache/` under the cache root — `<app-data>/waveflow/` by default, or wherever *Settings → Storage and backups → Folders and caches* moved it) and serves the on-disk copy instead — the same mechanism (and shared `motion_cache` primitives) as the [motion-artwork cache](plugins.md#apple-motion-artwork-metadata-world), a separate dir. The first consumer is a separate **unsigned** Spotify Canvas plugin — grey-area of Spotify's Developer Terms, isolated exactly like the excluded YouTube path, never the signed core.

**Sourcing — the bound server.** A track playing from the server has no local file, so neither of the two above applies: `track.id` is a **negative sentinel minted per playback**, and asking `get_track_canvas` about it would be querying the local database for a rowid that does not exist. [`useTrackCanvas`](../../src/hooks/useTrackCanvas.ts) branches on `remote_id` instead and asks [`remote::canvas`](../../src-tauri/crates/app/src/remote/canvas.rs), which mints a **ticket** (`POST /tracks/{id}/canvas-ticket`) and returns a self-authorising `/api/v2/canvas-stream/{ticket}` URL — a `<video src>` cannot send a Bearer header, the same problem remote audio solves the same way. The URL is always built from **our** base and a relative reply is required, since an absolute one would point the webview at a host the user never authenticated against. A server clip takes the rung the manual local one holds (somebody put it on that library on purpose); no Canvas on the server is an ordinary 404 that falls through to the plugin. The ticket expires within the hour, which is why nothing caches it durably — and the per-playback sentinel makes that automatic, since replaying the track mints a fresh one. Full rationale in [RFC-005](../rfcs/RFC-005-remote-source-and-sync-v2.md#a-server-tracks-canvas-rides-a-ticket-like-its-audio).

**"Show Canvas" toggle** — a [`CanvasToggleButton`](../../src/components/player/CanvasToggleButton.tsx) (Spotify's control) appears in the immersive top bar **and** the `NowPlayingPanel` header **only when the current track has a Canvas** and motion isn't reduced, so it is never a dead control. It drives the global [`useCanvasEnabled`](../../src/hooks/useCanvasEnabled.ts) preference (localStorage, **default OFF** — the cover shows first, the clip takes over on click, matching "click Show Canvas to reveal"). `prefers-reduced-motion` ([`usePrefersReducedMotion`](../../src/hooks/usePrefersReducedMotion.ts)) suppresses the clip and hides the toggle; radio (negative sentinel id) and Spotify tracks are excluded. Setting a Canvas is immersive-only in v1 (the panel only reflects + toggles).

**The clip's own frame (#694)** — Canvas is a 9:16 medium, and `object-cover` into the square cover frame drops about 44 % of the clip's height, split evenly top and bottom: any clip whose subject is not in the vertical middle comes out beheaded. It is not a crop to nudge, so in the `NowPlayingPanel` the frame **takes the clip's shape instead of the cover's**. [`CanvasStage`](../../src/components/player/CanvasStage.tsx) reports `videoWidth / videoHeight` through `onAspect`, tagged with the path it measured so an answer about a clip the surface has moved off is ignored rather than applied to the next one; it fires on `canplay`, not `loadedmetadata`, so the frame changing shape and the video fading in are one movement instead of a square cover sitting in an already-tall box. The frame is sized by a **definite width** (`min(100%, 56vh × ratio)`) with `aspect-ratio` deriving the height — a `max-height` cap would just squash the box out of ratio — and only a clip meaningfully taller than square takes it over. The still cover behind fills the tall frame rather than sitting square at the top, and the title / artist / album block moves **onto the clip's lower part over a scrim**, the way the reference does it: with the square frame gone nothing else holds that space, and white on a scrim is the only pair that is safe over colours we cannot know in advance. **The immersive hero stays square**: that column does not scroll and the hero shares its height with the metadata and the transport, so a frame tall enough for a vertical clip would push the controls off a short window.

**Lookup correctness** — [`useTrackCanvas`](../../src/hooks/useTrackCanvas.ts) dedupes the per-track lookup process-wide (the several surfaces resolve it at once, like [`useAlbumMotionArtwork`](../../src/hooks/useAlbumMotionArtwork.ts)) with three guards: the returned state is tagged with `{ id, profileId }` so a render never surfaces the previous track's/profile's clip during the async gap; a per-`trackId` generation so a set/clear invalidation can't be overwritten by an in-flight request; and a **monotonic profile-generation token** so a late completion from a prior profile can't repopulate the shared cache after a switch (track ids are per-profile). The picker opens against a **frozen** target track id + `hasCanvas` snapshot, so an auto-advance mid-dialog can't redirect a set/remove onto the wrong track. i18n under `canvas.*` + `canvasPicker.*` (the word "Canvas" kept verbatim across locales).

## Cover slideshow

[`CoverSlideshow`](../../src/components/player/CoverSlideshow.tsx) gently crossfades the album cover with the **artist photo** behind the now-playing view (issue #466, idea from @jo-el414) — a living backdrop built entirely from images already in the library, **no plugin**. A single artist overlay fading its opacity IS the crossfade: the static cover shows through whenever the overlay is at 0, so it reads cover → artist → cover, holding ~20 s each (starting on the cover). Rendered in [`ImmersiveNowPlaying`](../../src/components/player/ImmersiveNowPlaying.tsx) + [`NowPlayingPanel`](../../src/components/layout/NowPlayingPanel.tsx).

It sits one rung below the motion cover in the backdrop precedence — **Canvas > motion cover > cover slideshow > static cover** — so a surface only runs it when no [Track Canvas](#track-canvas) and no [motion cover](plugins.md#apple-motion-artwork-metadata-world) own the slot (each surface reads `useAlbumMotionArtwork` — deduped — purely for that gate).

**Three surfaces, one gate.** The immersive view, the now-playing panel and — since #702 — the [mini-player](#mini-player) all fold the toggle, `prefers-reduced-motion`, the track's eligibility (streamed tracks have no library artist) and the artist photo into the one boolean `CoverSlideshow` takes. That fold lives in [`useSlideshowLayer`](../../src/hooks/useSlideshowLayer.ts): it had been copied twice, and a third copy is how three surfaces drift apart. A surface that draws the rungs above passes `blocked`; a surface that already resolved the photo for its own reasons (the panel enriches the artist for "About the artist" anyway) passes `artistSrc`, and `undefined` rather than `null` is what means "resolve it yourself" — `null` is a real answer. The mini-player runs in its own webview, so its photo is a second `enrich_artist_deezer` call for the same artist: a hit on the shared `app.metadata_artist` cache, one IPC round-trip per artist change, and only for a profile that turned the slideshow on. It asks for `"full"` like the others — the pre-resized variants are 64×64 and 128×128 thumbnails, so there is no middle size to economise with.

**Toggle + guardrails** — a per-profile preference [`useCoverSlideshow`](../../src/hooks/useCoverSlideshow.ts) (`ui.cover_slideshow`, **default OFF**), toggled in Settings → Appearance → Immersive view via [`CoverSlideshowCard`](../../src/components/views/settings/CoverSlideshowCard.tsx); the write machinery (serialized writes, profile-switch guards, rollback, broadcast) mirrors [`useScrollLongTitles`](../../src/hooks/useScrollLongTitles.ts). `prefers-reduced-motion` suppresses the alternation, and a missing artist photo falls back to the static cover — so the feature is purely additive. The artist image is resolved through [`useArtistImage`](../../src/hooks/useArtistImage.ts) at **`"full"`** resolution (matching the artist detail page — a 1x thumbnail would upscale blurry in the large cover slot), and the enrichment fetch is gated on the toggle so it costs nothing while off. **The artist's own image wins over the Deezer one** — an `artist.jpg` sidecar the scanner linked, or a picture set from the [image picker](library.md#manual-override) — which is the order the artist page and the library grid already used; the hook read the enrichment alone until issue #701, so a curated photo was ignored when Deezer had one, and the slideshow had nothing to alternate with when Deezer had none. `enrich_artist_deezer` carries both (`artwork_path*` beside `picture_path*`), so one call answers for both sources, and the hook re-resolves on `artist:updated`. i18n under `settings.coverSlideshow.*`.

## Artist hero

[`ArtistHeroBackdrop`](../../src/components/common/ArtistHeroBackdrop.tsx) paints a **full-bleed backdrop behind the artist detail header** (issue #482), the Spotify artist-banner look — replacing the flat surface that only carried a circular avatar + name. Mounted by [`ArtistDetailView`](../../src/components/views/ArtistDetailView.tsx), which wraps its header in a `-mx-8 -mt-8` block to break out of `<main>`'s `p-8` so the image reaches the column edges (and shrinks with the column when a right panel opens).

**Two image tiers plus a no-image case, in precedence order:**

1. **A wide image** — the one the user [chose](library.md#choosing-the-banner) if there is one, otherwise `strArtistFanart` (or its alternates) from TheAudioDB, downloaded into the shared `metadata_artwork/` cache. Shown nearly crisp: `blur(2px)` only, enough to keep JPEG artefacts from crawling under the header copy. See [the backend pipeline](library.md#wide-artist-fanart-hero). The 1000×185 wide thumb and banner are **no longer used** — stretched across this header they were the "blurry backdrop" of issue #693.

   **Cropped from the top third, not the middle** (`background-position: center 30%`, same issue). `bg-cover` fills a very wide, fairly short banner, so the image is cropped top and bottom, and centring crops *equally* from both — which takes the head first, since faces sit in the upper third of nearly every artist photo. It also made the framing visibly shift as the window resized: a wider window makes the banner relatively shorter, so the crop deepens. Anchoring high loses sky and floor first. The blurred tier below keeps `center`, having no subject left to protect.
2. **The square artist photo** — Deezer picture or a local `artist.jpg`, heavily blurred + upscaled (`blur(56px) saturate(190%)`, `scale(1.35)`), the same colour-field treatment [`SkinAmbientBackdrop`](../../src/components/layout/SkinAmbientBackdrop.tsx) uses. Always available and **works offline**, which is why it's the universal fallback — a 1:1 image stretched across a banner would be unreadable unblurred.
3. **Nothing** — an artist with no image at all keeps today's flat header.

**Legibility** is not left to the theme: the image always carries a dark scrim (`from-black/85 via-black/60 to-black/35`) and the header copy (eyebrow / name / stats) is forced **white in every theme**, matching Spotify — whose artist header is dark-on-image in light mode too. The secondary buttons swap to a translucent white treatment over the hero. The bottom edge fades out through a **mask** (`linear-gradient(to bottom, black 68%, transparent)`) rather than a hard-coded colour stop, so the hero dissolves into whatever the current theme × skin paints behind it.

**Toggle + guardrails** — per-profile preference [`useArtistHero`](../../src/hooks/useArtistHero.ts) (`ui.artist_hero`, **default ON** — it's a baseline visual, not extra motion), toggled in Settings → Appearance → Library pages via [`ArtistHeroCard`](../../src/components/views/settings/ArtistHeroCard.tsx); the write machinery (serialized writes, profile-switch guards, rollback, broadcast) mirrors [`useCoverSlideshow`](../../src/hooks/useCoverSlideshow.ts). `prefers-reduced-motion` skips the `artistHeroFadeIn` cross-fade only — the image itself is static, so there is nothing else to suppress. The fanart source is seeded from `get_artist_detail` (metadata cache, first frame) and refined by the later `enrich_artist_deezer` response, which only ever _sets_ it: a refresh that comes back empty (offline, TheAudioDB down) must not blank a hero the cache already produced. Both of those resolve a **chosen** banner ahead of the cached one, for that same reason — the second one to answer decides what is on screen. i18n under `settings.artistHero.*` and `artistBackdropPicker.*`.

## Window frame

Who draws the bar at the top of the window — the desktop, or WaveFlow (issue #696). **Settings → Appearance → Player and window**, [`WindowChromeCard`](../../src/components/views/settings/WindowChromeCard.tsx), stored app-wide in `app_setting['ui.window_chrome']` (`system` — the default — or `app`).

The request was for the appearance choice Chrome offers on Linux, and it does not translate one-to-one: Chrome paints its own chrome from the GTK or Qt theme, while everything inside our window is a web view we already theme. What the desktop owns is the frame, so the frame is what this switches. A Qt mode in particular cannot mean Qt widgets — wry is bound to WebKitGTK and there is no Qt backend ([B1](../upstream-blockers.md#b1--webkit2gtk-41--gtk3-chrome-on-linux)).

**The platforms differ in kind, not in degree**, so [`commands/preferences.rs`](../../src-tauri/crates/app/src/commands/preferences.rs) resolves the stored choice into one instruction the interface follows, rather than leaving three platform tests in the layout:

| Platform | `app` means | What the frontend draws |
| --- | --- | --- |
| Linux | `set_decorations(false)` | [`AppTitleBar`](../../src/components/layout/AppTitleBar.tsx) — drag region, title, minimise / maximise / close |
| macOS | `TitleBarStyle::Overlay` + an emptied title | a 28 px drag strip, so the real traffic lights float over our own top bar |
| Windows | — | nothing: the option is not offered |

- **Windows is excluded on purpose.** A frame we drew ourselves would lose Snap Layouts and the system menu — a worse Windows than the one the user has, and nobody asked. `supported: false` hides the card, and the resolver refuses `app` there even if the row says otherwise (a database carried over from a Linux install).
- **macOS keeps the system's buttons.** Dropping the decorations would take the traffic lights with them, and drawing our own is the one thing a macOS user would call *not* native. An overlay title bar still paints the window title over our content and `hiddenTitle` is creation-time only, so the title is what we empty — and restore with the frame; the app is named by the menu bar either way. The overlay style also provides no drag region of its own, which is what the 28 px strip is for.
- **Applied before the reveal.** The main window is created hidden ([splash handoff](#surviving-a-renderer-that-cannot-paint-595)), and `restore_bounds_and_reveal` puts the chrome on just before showing it, so nobody watches the frame they turned off appear and vanish. `set_window_chrome` applies *and then* persists — in that order, so a frame that refused to change is never recorded as the one in use, and on macOS because the style and the title are two calls that must move together.
- **What is reported is what the window accepted.** A failure at startup logs and sets a session flag; `get_window_chrome` then answers `system` while leaving the stored preference alone. Without it the interface would draw a title bar over the system one the window in fact still has — two title bars, from a preference nobody could honour. The flag clears as soon as a `set_window_chrome` succeeds.

## Mini-player

[`MiniPlayerApp`](../../src/MiniPlayerApp.tsx) + [`MiniPlayer`](../../src/components/views/MiniPlayer.tsx) ship a Spotify-style always-on-top widget. Launched from the picture-in-picture button in the PlayerBar via [`lib/miniPlayer.ts::openMiniPlayer`](../../src/lib/miniPlayer.ts).

- **Window** — second `WebviewWindow` (label `mini`), default 280×380 with `decorations: false` (we render our own top bar) and `alwaysOnTop: true`. Hides the main window on open; the mini's Maximize button restores it and closes the mini.
- **Persistent bounds** — position + size are persisted in `app_setting['mini_player.bounds']` (JSON blob, machine-level) via debounced `onMoved` / `onResized` listeners in [`MiniPlayer.tsx`](../../src/components/views/MiniPlayer.tsx) (300 ms after the last gesture so SQLite isn't hammered at 60 Hz while dragging). On open, [`miniPlayer.ts::openMiniPlayer`](../../src/lib/miniPlayer.ts) restores the saved rectangle when it still overlaps an available monitor by at least 80 px on both axes (`availableMonitors()` check guards against monitor disconnects / resolution changes). Otherwise it falls back to anchoring bottom-right of the primary monitor (`currentMonitor` → physical size ÷ scale factor → logical px) with a 24 px edge margin so the OS taskbar / Dock isn't covered.
- **Routing** — same Vite bundle, branched in [`main.tsx`](../../src/main.tsx) on `?mini=1` so the mini boots into a stripped-down provider tree (`Theme + Profile + Player` only — no `Library` / `Playlist` since the widget never browses).
- **Cover-derived background** — [`lib/dominantColor.ts`](../../src/lib/dominantColor.ts) draws the artwork onto a 64×64 canvas, samples every 4th pixel, skips near-monochrome runs (white margins, black bars) so the average reflects the real hue, and produces a 3-stop gradient applied to the window background.
- **Hover overlay controls** — shuffle / prev / play (white round Spotify-style) / next / repeat fade in over the cover, with a compact **volume slider + mute** on a second row (#511); idle state shows just the artwork. The overlay also reveals on `focus-within` — its controls stay in the tab order, and a keyboard user shouldn't be driving a slider they can't see.
- **Shared state is broadcast, not per-window** — one engine behind two `PlayerContext`s, so anything reachable from both windows travels as an event: `player:volume-changed` from [`player_set_volume`](../../src-tauri/crates/app/src/commands/player.rs), `player:options-changed` from the shuffle / repeat commands, and `track:liked-changed` from [`toggle_like_track`](../../src-tauri/crates/app/src/commands/track.rs) (#523). The heart is shared through [`useLikedTracks`](../../src/hooks/useLikedTracks.ts), which the player bar and the mini-player both use rather than each keeping their own set. Only volume needs an echo guard — see [`invariants.md`](../architecture/invariants.md#events).
- **Drag region** — `data-tauri-drag-region` on the central dot strip, plus an explicit `getCurrentWindow().startDragging()` `onMouseDown` as a belt-and-suspenders fallback for the Windows hit-test races. Requires `core:window:allow-start-dragging` in the capability (not in `core:default`).
- **Pin toggle** — runtime `setAlwaysOnTop(bool)`; emerald when active.
- **Up-next overlay** — a `ListMusic` toggle in the top bar slides a translucent up-next list over the content area (the top bar stays reachable so the toggle still closes it). Backed by the same `player_get_queue` + `player:queue-changed` subscription as the main [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) (seq-guarded refetch), it lists every track after `current_index`; clicking a row calls `player_jump_to_index`, and finished tracks drop off as the index advances. Compact (no artwork, just position · title · artist). During a **remote session** it reads `remote_get_play_queue` and jumps with `remote_queue_jump` instead — the remote queue lives in memory on the backend, not in `queue_item`, the same split [`QueuePanel`](../../src/components/layout/QueuePanel.tsx) makes with [`RemoteQueueView`](../../src/components/layout/RemoteQueueView.tsx). It has no `queue-changed` event, so it refetches on the playing track id instead (#685).
- **Lyrics mode** (#580, reworked in #697) — a `Mic2` toggle in the top bar puts the lyrics **in the cover's slot** rather than on a sheet over the whole widget: the title, the seek bar and a transport row stay usable, so pausing or skipping no longer means closing the lyrics first. The cover it replaced sits behind the words, blurred, keeping the colour the window's gradient was sampled from. Synced LRC scrolls itself, centring the active line, and the active word carries the progressive karaoke fill through the shared [`useKaraokeWordFill`](../../src/hooks/useKaraokeWordFill.ts) — so the sweep stays continuous between the 4 Hz `player:position` events rather than stepping every 250 ms. Clicking a line seeks to it. Unsynced payloads and radio sessions render as plain text.
  - **Few lines, one obvious.** Every line used to be 12 px, the active one set apart by a font weight — which at that size the eye cannot find, and which made the word-level fill invisible although it was wired in. The active line is now noticeably larger, its neighbours smaller and dimmer, the first and last fade out under a mask instead of being sliced in half, and half-height spacers at both ends let the first and last lines reach the centre like any other (a fixed padding would be a guess about a window height the user can drag). **Centred**, matching the [desktop lyrics](#desktop-lyrics) window — both are glanceable surfaces rather than reading columns; the side panel and the immersive column stay left-aligned. Under `prefers-reduced-motion` the auto-scroll jumps instead of gliding.
  - **No header row.** The top-bar toggle already shows which mode is open and closes it, so the `LYRICS` label and its close button were a row of a 280×380 window spent on nothing.
  - **Mounted only while open.** [`useTrackLyrics`](../../src/hooks/useTrackLyrics.ts) fetches on every track change, so leaving it mounted behind a closed overlay would fire a second `fetch_lyrics` per track from this webview on top of the main window's. Unmounting puts that cost behind the user's actual request; when both surfaces _are_ open, the backend cache serves the second call. The rework kept that — it takes the cover's slot, it does not sit mounted behind it.
  - **No editing surface.** At 280×380 (draggable down to 240×320) there is no room for the side panel's source label, provider picker or the import / refetch / clear actions — those stay in the main window.
- **One mode at a time** — up-next and lyrics both claim the content area, so `MiniPlayer` holds a single `MiniOverlay` slot (`"none" | "queue" | "lyrics"`) rather than a boolean each; two independent flags would let them stack. Mirrors how `PlayerContext` mutexes the main window's three right-edge panels. Only the up-next sheet marks the cover / title / seek subtree `inert` — it genuinely hides those controls, where lyrics leave them in reach.
- **The document never scrolls** — `main.tsx` stamps `mini-player-window` on the root and [`app.css`](../../src/app.css) pins `overflow: hidden` on `html` / `body` / `#root`, the same declaration the desktop-lyrics window carries. The widget's own lists hide their scrollbars, but the page had nothing of the sort, and the up-next sheet's entry animation starts at `translateY(8px)`: an element already pinned to the bottom edge, pushed 8 px past it, is 8 px of page overflow — enough to paint a bar down the right edge of a 280 px window, and nothing to do with the lyrics list that was blamed for it (#697).
- **Interactive seek bar** — slim white bar at the bottom, click/drag to scrub. Same `pointer capture` + local `dragMs` pattern as the main `ProgressBar`. Thumb + timestamps fade in on hover so the idle widget stays minimal.
- **Capabilities** — the mini-player's window label is added to [`capabilities/default.json`](../../src-tauri/crates/app/capabilities/default.json) so it inherits every command the main window has access to (no duplicated capability file, no per-window permission pruning).

## Desktop lyrics

A floating lyrics window over other applications (#582): the line being sung, and under it that line's translation or the next line. [`DesktopLyricsApp`](../../src/DesktopLyricsApp.tsx) + [`DesktopLyrics`](../../src/components/views/DesktopLyrics.tsx) in the webview, [`desktop_lyrics.rs`](../../src-tauri/crates/app/src/desktop_lyrics.rs) for the window.

- **Owned by the backend, not the frontend.** Unlike the mini-player, the window (label `lyrics`, `?lyrics=1`) is created in Rust, because three control surfaces must agree on it and one of them, the tray, has no webview: the player bar's "⋯" menu ([`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx)), Settings → Appearance → Desktop lyrics ([`DesktopLyricsCard`](../../src/components/views/settings/DesktopLyricsCard.tsx)) and the tray's two check items. The two webview surfaces call `open_desktop_lyrics` / `close_desktop_lyrics` / `set_desktop_lyrics_locked` and follow the `desktop-lyrics:state` broadcast (`{ open, locked, wayland }`) through [`useDesktopLyricsStatus`](../../src/hooks/useDesktopLyricsStatus.ts) instead of trusting their own last click. The tray goes straight to `desktop_lyrics::toggle_from_tray` / `toggle_lock_from_tray` in Rust, and every change runs through `publish`, which both emits that broadcast and calls `tray::sync_desktop_lyrics` to set the tray's check marks from the same state — because a check item flips its own mark on click on some platforms before anything has happened. Open, close and the tray toggle decide under one lock, and an open that follows a close waits (up to 3 s) for `Destroyed`: `close()` only requests it, so the old window would otherwise still be found, merely shown, and then vanish. Past the wait the open fails rather than report a window that is about to go.
- **Window** — transparent, undecorated, `shadow(false)` (the drop shadow draws a grey box around floating text), always on top, on every workspace, out of the taskbar, opened **without focus** so it never takes the keyboard from the app the user is in. Independent of the main window: it stays up while the main window is hidden to the tray. Like the mini-player it only reads playback state, so it is not gated on Spotify mode; [`windowRole.ts`](../../src/lib/windowRole.ts)'s `IS_SECONDARY_WINDOW` keeps the Web Playback SDK, which can attach to one webview only, out of both secondary windows.
- **Transparent document** — `index.html` stamps `data-skin` in every window and the skins paint `body` from it, so `html.desktop-lyrics-window` in [`app.css`](../../src/app.css) forces the document transparent with `!important`. `main.tsx` adds the class before the first render; added from an effect, the first frame was an opaque box.
- **Lock (click-through)** — `set_ignore_cursor_events`. A locked window receives no mouse input at all, so nothing inside it can offer a way back: unlocking is only ever done from outside (tray, "⋯" menu, Settings), and every surface that can lock it can unlock it. A newly created window always starts **unlocked**, so closing a locked overlay never hands the user back one they cannot move.
- **Unlocked** — hovering or keyboard focus shows a frame, a lock button and a close button (mounted whenever unlocked, so they stay in the tab order), and raises the background to at least 35 % so the window's extent is visible; dragging anywhere moves it.
- **What it shows** — synced lyrics only, from the shared [`useTrackLyrics`](../../src/hooks/useTrackLyrics.ts). Word-timed lines take the progressive fill from [`useKaraokeWordFill`](../../src/hooks/useKaraokeWordFill.ts), with words already sung in the highlight colour; line-timed lines are drawn whole in it. Unsynced lyrics cannot follow the song, so title and artist stand in, as they do before the first line.
- **Fitting a line** — both lines use `leading-snug` and `em` padding inside their `truncate` box, so descenders and the outline are not clipped (with `leading-tight` the bottom of `g`, `y`, `p` was cut off on Linux, #673). A line too long for the window shrinks to fit, down to 60 % of its size, measured at the chosen size so it cannot oscillate; only past that floor is it cut with an ellipsis.
- **Appearance** — `profile_setting['ui.desktop_lyrics_style']` (JSON: font size 20–72 px, text colour, sung colour, outline, background opacity 0–80 %, show translation) through [`useDesktopLyricsStyle`](../../src/hooks/useDesktopLyricsStyle.ts). The outline is eight `text-shadow`s rather than `-webkit-text-stroke`, which eats into thin glyphs and needs `paint-order` that HTML text does not honour everywhere.
  - **Cross-window sync.** `useProfileSetting` broadcasts a window event, which never leaves its document, and this setting is edited in the main window while it is read in the overlay. A write re-emits it as the Tauri event `desktop-lyrics:style-changed`, which every *other* window turns back into the window event. The payload names the writer, and the writer ignores its own: while a slider is dragged the next write is queued, and a re-read landing in between would put the older stored value back on screen.
- **Bounds** — `app_setting['desktop_lyrics.bounds']`, saved 300 ms after the last move or resize like the mini-player's, read by Rust on open. A saved rectangle is used only while it overlaps a monitor by 80 px on both axes; otherwise the window opens bottom-centre of the primary monitor, 120 px above the bottom.
- **Wayland** — a native Wayland client cannot hold "always on top" or its own position if the compositor declines, and GNOME's Mutter declines both (confirmed on Fedora, #673). The window still opens and can be dragged. Settings shows a note when the backend detects a Wayland session (`XDG_SESSION_TYPE` / `WAYLAND_DISPLAY`, unless `GDK_BACKEND=x11` forces XWayland) and gives the workaround: start WaveFlow with `GDK_BACKEND=x11`, where Mutter honours always-on-top for the X11 client. Forcing XWayland from the app was ruled out: `GDK_BACKEND` is process-wide, so it would blur fractional scaling and drop native Wayland behaviour for every Linux user, overlay or not.
- **macOS** — a transparent window needs the `macos-private-api` Cargo feature and `app.macOSPrivateApi` in `tauri.conf.json`. It rules out the Mac App Store, which WaveFlow does not ship through.

## Splash screen

To hide the cold-start delay (Windows SmartScreen / Defender scanning every freshly-extracted DLL on the very first launch after install, plus the `setup()` chain in [`lib.rs`](../../src-tauri/crates/app/src/lib.rs) — opening `app.db` + running migrations, creating the default profile, cold-initialising cpal/WASAPI), the main window is created with `"visible": false` and a small secondary window (`label: "splashscreen"`, 360×240, opaque `#121212`, decorations off, always-on-top, off the taskbar) shows a WaveFlow logo + indeterminate progress bar while the backend boots and the React bundle parses.

The static HTML lives in [`public/splash.html`](../../public/splash.html) (no JS, inline SVG logo, single CSS animation) so it paints the instant the WebView2 process spawns. The splash → main handoff is **driven from the backend** — the frontend's [`ReadySignal`](../../src/components/common/ReadySignal.tsx) component reports readiness after React's first commit (via `useEffect`, not `requestAnimationFrame` — WebKitGTK 2.52 suspends rAF callbacks while a window is `visible: false`), primarily through the `app_ready` command and also as an `app://ready` event. Both land on [`commands::ready::signal`](../../src-tauri/crates/app/src/commands/ready.rs), which releases the rendezvous a single task in [`lib.rs`](../../src-tauri/crates/app/src/lib.rs)'s setup is waiting on; that task then calls `reveal_main_close_splash`: show main first, set focus, then close the splash so the desktop is never visible between the two. A 15 s safety-net timer + bounded retry loop (10 attempts, 250 ms backoff) revives the handoff if the signal never arrives.

That net fires regularly on Windows, and when it does the user watches the splash for a full fifteen seconds (#626). Two things changed while the cause is still being pinned down. The signal's **primary transport is now the `app_ready` command** ([`commands::ready`](../../src-tauri/crates/app/src/commands/ready.rs)), retried a few times frontend-side: `generate_handler!` registers a command at build time, whereas that `app.listen` is registered part-way through `setup` — after three blocking database reads and the audio device open — while the `main` webview, created from the config by `Builder::build`, is already running. Events are not replayed, so one handled before the listener exists is gone. The event stays as a second transport; the rendezvous is one-shot and ignores duplicates, so the splash is not worth a single point of failure.

And **both sides now report their timing**: the frontend sends `performance.now()` at its first commit, the backend logs it beside its own elapsed-since-launch. That is what tells "the frontend was genuinely late" apart from "the signal was lost" — the leading suspect for the first being that `main.tsx` gates its first render on i18next, whose locale chunk is fetched through the asset protocol that `setup` is blocking. Until a log settles it, the 15 s net stays exactly as it is: it is what keeps a launch merely slow instead of stuck. The mini-player webview branches out via `?mini=1` and skips the dance.

### Surviving a renderer that cannot paint (#595)

The same signal answers a second question. When GPU-accelerated rendering fails, the app starts and shows nothing — the window exists, the process is alive, and there is a blank rectangle with no message and no way back. So a launch **arms a marker** before the window is created and disarms it on that first committed render; a launch that finds the marker still armed knows the previous one never got that far and starts in software rendering instead. [`render_mode`](../../src-tauri/crates/app/src/render_mode.rs) holds it, called from `run` before `tauri::Builder` is constructed — like the schema guard, and for a sharper reason: the variables it sets are read by the web engine when *its* process starts, and the `main` webview is created by `Builder::build`, before `setup` runs at all.

What it catches is worth stating precisely: the marker is disarmed by that first committed render, which is the strongest signal this side has and is **not** the same thing as pixels reaching the screen. A failure that leaves JavaScript running while nothing composites would still report a paint. Waiting for the native reveal instead would not help — `show()` returning `Ok` proves no more about pixels than a React commit does — so what this survives is the shape actually reported: a launch that opens a window and never renders into it.

The 15-second safety net is the other side of that. It reveals the window when no signal arrives and deliberately does **not** disarm the marker: a renderer that cannot paint produces exactly that timeout, so disarming there would make the whole thing inert. The cost is that a signal lost for an unrelated reason reads as a launch that never painted, and the one after it falls back — which is the case the banner explains and the retry button undoes, not a silent downgrade.

**Paint is the only thing that disarms it.** Not window close, not process exit: clearing the marker when the user closes the blank window would let them erase the evidence, and the next launch would try the GPU again — the loop repeating forever while the mechanism appears not to work.

**The fallback sticks**, because a one-shot one would be worse than none: the software launch paints, the marker clears, and the launch after it is blank again — every other start. A software launch that painted records that it was software, and Settings → Maintenance → Diagnostics offers the way back ("Try the GPU again", applied on the next start). The exception is software *also* failing to paint: the GPU was then not the problem, so that returns to the default rather than degrading rendering for a fault it does not address.

What "software" sets, measured against the engines in play rather than copied from advice: on **Linux** `WEBKIT_DISABLE_COMPOSITING_MODE` and `WEBKIT_DISABLE_DMABUF_RENDERER` (both read by the installed WebKitGTK 2.52, checked in its symbol table) plus Mesa's `LIBGL_ALWAYS_SOFTWARE`; on **Windows** `--disable-gpu` appended to `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, with the limitation Microsoft documents — **an elevated app ignores flags set that way**; on **macOS** nothing, because WKWebView has no equivalent and half an answer would be worse than an honest none — so every route into software answers `software-unavailable` there instead, rather than announcing a downgrade that never happened and storing a state the GPU never gets tried out of. A variable the user already exported is left as it is — except the Windows one, which has to change because it is a list: the flag is appended to whatever is already in `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, so nothing they put there is lost.

**A startup that stops takes the marker down with it**, whatever stopped it. The decision runs before the schema guard now — it has to precede the logger, which the fatal paths need in order to explain themselves — so a database written by a newer build would otherwise leave a marker armed and send the launch after it into software rendering over a schema version. And `setup` has a dozen fallible steps after that (a tray menu item, the window icon, the tray itself), each of which aborts the launch before anything can paint: a guard covers all of them at once, including the ones added later, because a call at each `?` is a call the next one does not get. The two paths that leave through `std::process::exit` disarm explicitly, since that runs no destructors.

One more thing the marker had to learn: **a second launch arms it too.** The single-instance plugin turns a duplicate away from inside `Builder`, which is after the marker is written — so opening the app twice left one armed by a process that was never going to paint, and pushed the launch after it into software for no reason. The plugin's callback runs in the instance that stays, and that instance writes back what it knows.

It writes what it decided, not what the file says by then, and that distinction is load-bearing: the duplicate runs the whole decision too, reads the marker the first one armed, and on a software session concludes that software did not help — erasing a working fallback that belonged to a process still starting up. Every write after the decision uses the value read at decision time, so the duplicate's guess cannot outlive it.

`WAVEFLOW_RENDERER=gpu|software|auto` overrides all of it, for someone who has been told what to try. A forced mode does not offer the undo button: the stored state is not what is deciding, so the button would change nothing.

Why backend-driven: v1.1.0 ran the handoff entirely in `main.tsx` via `requestAnimationFrame` + IPC `window.show()` + `splash.close()`. On Linux WebKitGTK 2.52+ the heavy first-launch init (migrations + DB pool + WebKit profile dir) raced the rAF window, the show()/close() could fire on a non-ready webview, and the user was stuck on an eternal splash (issue #42). Native-side ownership + an explicit "DOM committed" signal is robust to that race.

Splash window background is opaque on purpose: `"transparent": true` forces an alpha-capable EGL config that some WebKitGTK builds reject, doubling the EGL failure surface (see also the AppImage incompatibility note for WebKitGTK 2.52+).

## System tray

Quick playback controls (Play/Pause, Previous, Next, Quitter), and the desktop lyrics window's show and lock check items ([Desktop lyrics](#desktop-lyrics)). Close-to-tray is the default close behaviour — the `WindowEvent::CloseRequested` handler hides the window unless the tray "Quitter" item armed `QuitGate`. Tray ID is `waveflow`.

## Taskbar thumbnail buttons (Windows)

Hovering the taskbar icon shows Previous / Play-Pause / Next under the window preview ([`taskbar_buttons.rs`](../../src-tauri/crates/app/src/taskbar_buttons.rs), #583). No progress bar and no overlay badge on the taskbar icon: only the three buttons.

- **Clicks** reach the window procedure as `WM_COMMAND` / `THBN_CLICKED`. Tauri has no hook for that (tao's `msg_hook` is Tauri's own, and only sees posted messages), so the main window is subclassed with `SetWindowSubclass` from `setup`, on the thread that created it. Clicks go through [`player_actions`](../../src-tauri/crates/app/src/player_actions.rs) like the tray's.
- **The toolbar goes on at `TaskbarButtonCreated`**: before the taskbar button exists, `ThumbBarAddButtons` fails. `main` starts hidden, so that is the splash handoff.
- **Play/pause follows `player:state`**, the event the in-app button follows, not the last click. `loading` is ignored so the icon doesn't flicker between two tracks.
- **Icons** are drawn at runtime with `tiny-skia` at the small-icon size, white on a dark taskbar and near-black on a light one (`SystemUsesLightTheme`), and redrawn on `WM_SETTINGCHANGE`.
- **Tooltips** ride on the tray's label push (`set_tray_labels`) and reuse `player.controls.{play,pause}` plus the tray's previous / next strings, so no new keys.
- Every COM and icon call stays on the window's thread; the `player:state` listener and the label command only update shared state and post a refresh message to the window.

## Statistics view

[`StatisticsView.tsx`](../../src/components/views/StatisticsView.tsx) projects from `play_event`:

- KPIs (total listening time, distinct tracks/artists/albums, completion rate). Each card carries a stable id and the user can hide any of them from **Settings → Appearance → Library pages** ([`StatsKpiVisibilityCard`](../../src/components/views/settings/StatsKpiVisibilityCard.tsx)) — persisted per-profile in `profile_setting['stats.hidden_kpis']` as a JSON array of ids, read via [`useHiddenKpis`](../../src/hooks/useHiddenKpis.ts) (window-event broadcast so the view re-reads without a remount). Default = nothing hidden. Motivated by the "Full-listen rate" KPI feeling judgemental, but applies uniformly to every KPI.
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

The banner also exposes a per-recap-year dismiss button (the `X` in the top-right corner) that writes `profile_setting['wrapped.dismissed_year']` so a quick close hides the banner for that year only — next year's recap re-appears automatically. Mode is configured from Settings → Appearance → Library pages via [`WrappedBannerCard`](../../src/components/views/settings/WrappedBannerCard.tsx). The card also surfaces the current season status (`seasonActive` / `seasonIdle`) when `auto` is selected so the user understands why the banner is or isn't on their Home right now.

The full banner stack — visibility check + `available_wrapped_years` length — collapses to nothing when either condition is unmet, so an empty library never paints the banner regardless of mode.

### Shareable PNG

The Share button in the overlay top bar opens a two-action menu: **Save as PNG** (native save dialog → file on disk) and **Copy image** (clipboard via `navigator.clipboard.write` + `ClipboardItem`). Both go through [`lib/wrappedCard.ts`](../../src/lib/wrappedCard.ts), a pure Canvas 2D renderer that produces a 1080×1920 portrait PNG mirroring the overlay's visual style — radial-gradient backdrop sampled from the same accent palette, year + total minutes as marquee elements, top 5 tracks with cover thumbnails, mood + streak strip, "Powered by WaveFlow" footer. Text uses the WebView's native font stack so we don't ship a font file with the bundle. The "save" path serialises the PNG bytes through the IPC channel ([`save_share_image(bytes, target_path)`](../../src-tauri/crates/app/src/commands/share_image.rs), shared with the Now Playing card) and writes via `spawn_blocking` — no `tauri-plugin-fs` dependency. The "copy" path stays in the browser and works on Chromium-based WebView (Edge on Windows, WKWebView on macOS); WebKitGTK on Linux historically refused image/png clipboard writes, so the error is surfaced rather than silently no-op'd.

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
| **Primary**  | Lyrics, Queue, Device picker, "⋯", Volume, Mini-player, Immersive view, plus any pinned overflow item (A-B loop, Sleep timer, EQ presets). Every entry is opt-out from the user side via Settings → Appearance → Player and window (defaults match the pre-customisation layout — zero visible change after the upgrade). | Each button reads its visibility from `usePlayerBarLayout` ([`src/hooks/usePlayerBarLayout.ts`](../../src/hooks/usePlayerBarLayout.ts)). The same hook drives the live preview in the Settings panel.                           |
| **Overflow** | Playback speed (slider + presets), EQ presets, A-B loop, Sleep timer                                                                                                                                                                                                                                | [`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx) — "⋯" popover; trigger auto-hides when every overflow entry is pinned. EQ presets share their inner `EqPresetPanel` body with the primary popover variant. |
| **Pinnable** | A-B loop, Sleep timer, EQ presets (promote each to primary independently)                                                                                                                                                                                                                           | Settings → Appearance → Player and window → "Player bar layout" — single panel covering every button + the cover-click action.                                                                                                                        |

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

The opt-in audio-quality footer ([`AudioQualityFooter`](../../src/components/player/AudioQualityFooter.tsx), pinned via Settings → Appearance → Player and window → Player bar layout) is a thin strip below the player bar that surfaces the source file specs in compact form (`48 kHz · 256 kb/s · 6 Mo` on the left, `AAC · 24bit · 48kHz` on the right; bitrates ≥ 1000 kbps render as `Mb/s`). When the engine is resampling — source rate ≠ output device rate — the left chunk renders an arrow instead: `48 kHz → 44.1 kHz · …`, so the user can spot the conversion at a glance without opening the popover. The arrow is gated on the device rate being known (the engine reports `0` before the first stream opens); otherwise we fall back to the source rate alone rather than printing a misleading `48 kHz → null`. The Hi-Res pill surfaces when [`isHiRes`](../../src/lib/hiRes.ts) accepts the source bit depth / sample rate combination.

### Hi-Res / DSD badge

[`HiResBadge`](../../src/components/common/HiResBadge.tsx) is the green pill that decorates track rows, album grid tiles, and the player-bar metadata when the source qualifies as Hi-Res (`isHiRes` — ≥ 24-bit, ≥ 44.1 kHz) or as DSD (`dsdLabel` returns `DSD64` / `DSD128` / …). Three variants:

| Variant   | Used in                               | Style                                                                                           |
| --------- | ------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `overlay` | Album / artist grid covers (default). | Absolute-positioned pill in the cover's top-left corner with a drop shadow.                     |
| `inline`  | TrackTable rows, sidebar lists.       | Inline rounded pill next to the title.                                                          |
| `text`    | Player bar — under the artist name.   | Spotify-style minimal green uppercase text, no pill background, blends into the metadata stack. |

All variants are gated by [`useHiResBadgeVisibility`](../../src/hooks/useHiResBadgeVisibility.ts), which reads `profile_setting['ui.show_hi_res_badge']` (default `true`) and re-reads on the `waveflow:hi-res-badge-visibility` window event. Settings → Appearance → Player and window ships [`HiResBadgeCard`](../../src/components/views/settings/HiResBadgeCard.tsx) to flip the flag — when off, every mounted `HiResBadge` returns `null` in one render, including the player-bar text label. Per-profile so a kid's profile can hide the audiophile chrome while the audiophile profile keeps it.

Hovering (or keyboard-focusing) the footer opens [`AudioPipelinePopover`](../../src/components/player/AudioPipelinePopover.tsx) — an audiophile-grade breakdown of what the engine is actually doing.

#### Sections displayed

- **Source** — codec, sample rate, bit depth, bitrate, channel layout (`Mono` / `Stereo` / `3.0` / `4.0` / `5.0` / `5.1` / `6.1` / `7.1`).
- **Processing** — chips lighting up for every active stage. The two conversion chips inline the actual delta so they match the footer's arrow notation: `Rééchantillonnage 48 → 44.1 kHz`, `Downmix 5.1 → Stereo`. The other chips stay as bare labels: `DSD → PCM`, `EQ`, `ReplayGain`, `Normalize`, `Mono` mixdown, `Speed ≠ 1×`. No chip → "Aucun traitement appliqué".
- **Output** — device sample rate + channel layout read from the live engine snapshot (`PlayerStateSnapshot.sample_rate` / `channels`), not the track row, so resampling and downmix are reflected correctly.

#### Bit-perfect conditions

Two things have to hold, and the pill used to check only the first:

1. **Nothing in our pipeline touches the samples** — no processing chip is active and the source rate matches the output rate. Any single chip lit (including `EQ` and `Speed`) suppresses it.
2. **Nothing downstream touches them either** — the stream owns the device (`PlayerStateSnapshot.exclusive_active` — WASAPI Exclusive, a raw ALSA `hw:` device or CoreAudio hog mode, whichever the platform has; native DoP implies an exclusive backend and qualifies on its own).

The second condition is what makes the claim true. A shared-mode stream at the same nominal rate still passes through the system mixer, which re-clocks it and mixes in every other sound on the machine — and that was being badged `Bit-perfect`. When the pipeline is clean but the device is shared, the pill reads `Sortie partagée (mixeur système)` instead, so the reason the green one is absent is on screen rather than left to guess.

#### State refresh

Hydration on open runs `playerGetState` / `playerGetAudioSettings` / `playerGetEq` in parallel so every read reflects the freshest engine state — the EQ may have been flipped from another popover seconds ago and we want truth, not stale React state. The popover unmounts on hover-leave so we never display stale data once it closes. 120 ms open / 200 ms close hover delays so brushing the footer doesn't flicker the popover open.

## Keyboard shortcuts

Action ↔ key bindings live in [`src/lib/shortcuts.ts`](../../src/lib/shortcuts.ts) (12 actions, defaults like `Space` → play/pause, `←`/`→` → previous/next, `M` → mute, `S` → shuffle, `R` → repeat, `L` → toggle lyrics, `Shift+L` → like). [`useGlobalShortcuts`](../../src/hooks/useGlobalShortcuts.ts) is mounted once in [`AppLayout`](../../src/components/layout/AppLayout.tsx) and attaches a single `window.keydown` listener that dispatches against `PlayerContext`. Listener skips when the focus target is `INPUT` / `TEXTAREA` / `contenteditable` so typing in a search box doesn't toggle shuffle.

User overrides are stored per-profile in `profile_setting['ui.shortcuts']` as a JSON object containing only customised actions — defaults stay implicit, so future default tweaks land for any binding the user hasn't touched. Settings → Keyboard shortcuts ([`ShortcutsCard`](../../src/components/views/settings/ShortcutsCard.tsx)) captures keys in capture-phase so the rebind UI doesn't fire the global handler. Conflicts auto-resolve by stealing the combo from whoever previously owned it. AboutView reads the same setting and re-renders on the `waveflow:shortcuts-changed` window event.

## Theming & motion

- **Dark mode** — animated radial transition via the [View Transitions API](https://developer.mozilla.org/en-US/docs/Web/API/View_Transitions_API). Falls back to an instant swap when unsupported.
- **`prefers-reduced-motion`** respected for the radial transition and for animated SVGs. Loading spinners stop too (one `.animate-spin` rule in [`app.css`](../../src/app.css)); that is why a busy button swaps its icon for `Loader2` instead of spinning its own icon — a stopped `Loader2` still reads as loading, a stopped refresh arrow reads as an idle button.
- **Single-click play** — optional Settings toggle; the default is double-click to mirror Apple Music / Finder.
- **Framer Motion** — `motion/react` provides micro-interactions (sidebar nav reorder, modal open, view fade-in, queue drag). One global [`SkinMotionWrapper`](../../src/components/layout/SkinMotionWrapper.tsx) feeds skin-specific `transition` config to the `MotionConfig` provider so per-skin springs (Pulse uses `cubic-bezier(0.34, 1.56, 0.64, 1)`, Lounge stays tame, etc.) apply automatically without touching call sites.

## High contrast

A third axis alongside theme and skin (#596): **`auto` / `normal` / `high`**, per profile, in Settings → Appearance → Theme and readability. `auto` is the default and defers to the OS `prefers-contrast: more`; the other two are explicit and win over the OS in both directions. [`applyContrast()`](../../src/lib/contrast.ts) writes the *resolved* state (`high` or `normal`, never `auto`) to `data-contrast` on `<html>`, so no stylesheet has to know what `auto` resolved to.

What the mode changes:

- **Secondary text**. Tailwind's `--color-zinc-{300,400,500,600}` are re-pointed at `--wf-zinc-*` through `@theme inline`, the same lever that lets a theme re-tint every `bg-emerald-*`. Those four shades are ~1 100 *text* uses and almost nothing else; 700-900 are deliberately left alone because each of them is a background, a border **and** text at once, and one value cannot deepen a surface and lighten a border at the same time. Light ground darkens, dark ground lightens — which means the dark-ground selector has to include `[data-skin="lounge"]` and `[data-skin="pulse"]`, since those two are dark by skin and never carry the `.dark` class.
- **Blur off**, plus opacity. `backdrop-filter` is reset globally, and every surface that was legible *only* thanks to its blur is forced opaque in the same breath — `.wf-glass` for the app's own chrome, and a per-skin block in `liquid.css` / `pulse.css` / `lounge.css` for theirs. Skipping the second half is worse than doing nothing: Pulse's sticky header is `transparent` and Liquid's chrome is a 8-16 % tint, so unblurring alone puts their text straight onto the content scrolling underneath. Purely decorative translucency over a hero gradient is left as it is.
- **Focus** becomes a 3 px `outline` (not a ring — an outline survives an `overflow: hidden` ancestor).
- **Switches and checkboxes** gain an edge in both states and a mark in the on state, so "filled versus not filled" stops being the only signal. Keyed on `[role="switch"]`, which the shared [`ToggleSwitch`](../../src/components/common/ToggleSwitch.tsx) and every inline copy already carry.

The first paint is handled by the bootstrap script in `index.html` alongside the theme and skin stamps; [`ContrastProvider`](../../src/contexts/ContrastContext.tsx) re-stamps it from the profile row a few ms later, and is mounted in the mini-player's tree too — that window has its own document, so it needs its own stamp.

## Skins

Skins are an **orthogonal axis to the 14 colour themes**: a skin re-skins surfaces, typography, motion and signature elements (e.g. Editorial's drop caps, Pulse's vinyl-spin cover); the theme picks the OKLCH accent. Every skin × theme combination is valid → **5 × 14 = 70 visual identities**.

<!-- markdownlint-disable MD060 -->

| Skin        | Direction                            | Signature                                                                                                                                                                                                                                                                             |
| ----------- | ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `studio`    | Apple Music baseline                 | Inter / system-ui, soft shadows, rounded-12 — the calm default                                                                                                                                                                                                                        |
| `editorial` | Broadsheet "WaveFlow Gazette"        | Playfair Display 900 + Lora body, `<EditorialMasthead>` with locale-aware `Intl.DateTimeFormat`, `::first-letter` drop caps, halftone N&B covers via `mix-blend-luminosity`, ruler-tick progress bar via `repeating-linear-gradient`, `counter-reset` page numbering ("P. 1", "P. 2") |
| `lounge`    | "Listening Room" warm-burgundy glass | Inter, [`SkinAmbientBackdrop`](../../src/components/layout/SkinAmbientBackdrop.tsx) blurs the cover (`filter: blur(140px) saturate(280%)`) into the body, white-overlay tint over the cover dominates the chrome                                                                      |
| `pulse`     | OLED "club control panel"            | Space Grotesk + Space Mono, dual-neon magenta/cyan aurora blobs, `///` mono pills, vinyl-spin cover via [`SkinPlayingState`](../../src/components/layout/SkinPlayingState.tsx) mirroring `usePlayer().isPlaying` to `[data-is-playing]`, floating pill PlayerBar                      |
| `liquid`    | Apple Vibrancy material              | DM Sans Variable (`opsz` axis 9..40), 8-layer inset `box-shadow` recipe `--liquid-glass` for "real glass" surfaces, aurora drift `body::before` (28 s), theme-aware light/dark token swap via `:not(.dark)`, `--liq-action` cyan/blue swap for contrast                               |

<!-- markdownlint-enable MD060 -->

**Architecture** :

- Token system + `SkinId` union live in [`src/lib/skins.ts`](../../src/lib/skins.ts); `applySkin()` writes `data-skin` on `<html>`.
- Per-skin overrides live in `src/styles/skins/{editorial,lounge,pulse,liquid}.css` (Studio = no overrides, baseline). The four files are imported from `src/app.css`. Three of them also carry a high-contrast block (see above) — a skin that adds a glass surface has to add it there too, or that surface becomes unreadable in the mode.
- [`src/app.css`](../../src/app.css) extends the Tailwind `dark` variant via `@custom-variant dark (.dark, .dark *, :root[data-skin="lounge"] *, :root[data-skin="pulse"] *)` — Lounge/Pulse fire `dark:*` utilities automatically because they're always "dark" by design (Liquid stays theme-aware).
- **Local-first typography** — all skin fonts are bundled via `@fontsource` / `@fontsource-variable` (Playfair Display, Lora, Space Grotesk, Space Mono, DM Sans Variable) and imported at the top of [`src/main.tsx`](../../src/main.tsx). Zero network at runtime — no Google Fonts request.
- **Motion** — each skin declares its own `MotionConfig` spring in `SkinMotionWrapper`. Skins with strong identity (Pulse, Editorial) override the spring; calmer skins (Studio, Lounge, Liquid) inherit the soft default.

The picker lives in Settings → Appearance → Theme and readability via [`SkinPickerCard`](../../src/components/views/settings/SkinPickerCard.tsx), alongside the theme picker and the contrast setting. View-Transitions API also drives skin swaps (radial reveal on click), with the same try/catch fallback used by the theme picker for WebKitGTK builds.

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
- **Per-profile preferences go through [`useProfileSetting`](../../src/hooks/useProfileSetting.ts)** (issue #485) — one implementation of the pattern every `profile_setting`-backed hook needs, with a `useProfileBooleanSetting` shorthand for the on/off rows that make up most of Settings → Appearance. It owns: the optimistic update, a serialized write chain (rapid toggles land in click order), a token shared by reads and writes (a read in flight can't clobber a toggle fired meanwhile, and only the newest write broadcasts), rollback to the last **backend-confirmed** value rather than to the pre-toggle optimistic one, a `ready` gate, and a reset to the default on profile switch so the outgoing profile's value never paints. The hand-rolled copies had drifted into carrying different subsets of that list — `useScrollLongTitles`, `useArtistBioCollapsed`, `useHiddenKpis`, `useCoverSlideshow`, `useVisualizerColor`, `useArtistHero` and `useWrappedBannerVisibility` are now thin wrappers over it (the last one holds two keys, so it mounts two instances — each on its own broadcast channel, since a shared one would make a write to either key trigger a re-read of the other, which can land on top of that other key's in-flight optimistic value). Three preference readers keep their own implementation because their shape doesn't fit, not because they were missed: `useWebRadioFavorites` persists through the plugin-favorites commands rather than `profile_setting`, `useHiResBadgeVisibility` publishes through a module-level store so non-React callers can read it, and `useSortMemory` spans many dynamic keys.
- **The write is profile-scoped in the backend, not just in JS.** [`set_profile_setting` / `get_profile_setting`](../../src-tauri/crates/app/src/commands/profile.rs) take an optional `expected_profile_id` and validate it via [`AppState::require_profile_pool_for`](../../src-tauri/crates/app/src/state.rs), which compares and leases under **one** acquisition of the lock `switch_profile` takes to swap the pool. A frontend guard cannot close that window on its own: it necessarily runs before its `invoke` crosses the IPC boundary, so a switch landing in between still redirects the write. Omitting the id keeps the old behaviour (act on whatever is active) — right for a direct user action, wrong for anything queued behind an await. `ThemeContext` and `SkinContext` therefore capture `activeProfile.id` at the top of their rehydration effect and pass it to both calls: their `cancelled` flag is checked _before_ the seeding write, so without the id a switch landing during that IPC hop would seed one profile's theme into another.
- Profile switch closes the current per-profile pool and opens the new one — UI reactively re-fetches via every `*Provider` watching `activeProfile.id`. [`LibraryContext`](../../src/contexts/LibraryContext.tsx) also exposes `loadedProfileId` (the id its `libraries` array was last fetched for) so consumers like the onboarding gate in [`AppLayout`](../../src/components/layout/AppLayout.tsx) wait for a fresh fetch instead of evaluating against the previous profile's data. `refresh()` snapshots the active profile id before its `await` and drops late writes when the user has since switched.

### Create / delete

[`ProfileSelectorModal`](../../src/components/common/ProfileSelectorModal.tsx) hosts the lifecycle:

- **Create** → "+" tile in the select view → name + colour picker → backend [`create_profile`](../../src-tauri/crates/app/src/commands/profile.rs) reserves the row, materialises `profiles/<id>/`, and runs the initial migration. The freshly-created profile is auto-activated.
- **Delete** → Netflix-style "Manage" toggle (pencil ↔ check) in the top-left corner reveals a red trash badge on every non-active profile; tapping the badge or the profile card itself opens a destructive confirmation view. In this mode the card no longer switches profile, since it reads "Tap to delete" (#614). Backend [`delete_profile`](../../src-tauri/crates/app/src/commands/profile.rs) refuses the active profile and the last remaining profile. The guard is **atomic**: a single SQL statement (`DELETE FROM profile WHERE id = ? AND (SELECT COUNT(*) FROM profile) > 1`) couples the "must not be last" predicate with the mutation so two concurrent deletes can never empty the table. Disambiguation between "not found" and "last profile" is handled on the failure path. After the row is removed, `profiles/<id>/` is wiped from disk and `app.last_profile_id` is cleared if it pointed to the deleted profile.

### Export / import (`.waveflow` archive)

[`commands/profile_io.rs`](../../src-tauri/crates/app/src/commands/profile_io.rs) packages a profile into a single `.waveflow` (zip) file containing `manifest.json` + `data.db` + the per-profile `artwork/` directory. Settings → Storage and backups → Profile backups exposes both buttons.

- **Export:** the active-profile path runs `PRAGMA wal_checkpoint(TRUNCATE)` first so the bundled DB captures every committed page (otherwise a busy WAL would leave the archive holding a partial snapshot). The CPU-bound zip work runs on `tokio::task::spawn_blocking`.
- **Import:** always allocates a fresh profile row — never overwrites — then extracts the archive under `profiles/<new_id>/`. Failures roll the row back so a half-imported profile doesn't survive the error. Before the sqlx migrator runs, [`normalise_migration_checksums`](../../src-tauri/crates/app/src/commands/profile_io.rs) rewrites `_sqlx_migrations.checksum` for every version present in both the archive and the local migrator — older builds checked out migration files with CRLF endings (Windows `core.autocrlf=true` + no `.gitattributes` lock) so their stored SHA-384 differs from the same SQL re-hashed today, even though the DDL is identical. A `.gitattributes` at repo root now pins `*.sql` / `*.rs` / `*.ts` / etc. to LF so future archives stay byte-stable. Once normalised, the new pool is opened once so any pending sqlx migrations replay before the user switches to it. An archive whose `_sqlx_migrations` lists a version unknown to the local migrator is rejected — that means the export came from a newer build.
- **Out of scope:** the shared `app.db` (Last.fm key, Discord opt-in, `network.offline_mode`) belongs to the install, not the profile.
- **Shared artwork cache:** the `metadata_artwork/**` directory (Deezer pictures, etc.) rides along when `app_setting['backup.include_metadata_artwork']` is on — the **default**, since re-fetching it costs thousands of API calls and leaves the restored profile visually blank until they land. Turning it off keeps archives small at that price. In an auto-backup pass the cache is bundled into the **first** archive only, so N profiles don't each duplicate it.
- **Manifest:** `archive_version` (currently `1`) gates compatibility — a future schema-incompatible bump refuses imports rather than silently corrupting the new profile. `app_version` and the source profile name / id are recorded for diagnostics.

### Auto-backup

Opt-in scheduled mirror of the manual export so the user's playlists / likes / ratings / history survive a SQLite corruption or disk failure. Implementation in [`backup.rs`](../../src-tauri/crates/app/src/backup.rs):

- **Config** lives in `app_setting` (install-wide, not per-profile): `backup.enabled` (bool, default OFF), `backup.interval_days` (1-90, default 7), `backup.folder` (string; empty = default `<app_data>/waveflow/backups/`), `backup.retention` (1-50, default 5 — per profile), `backup.last_run_at` (epoch ms).
- **Loop** is a single tokio task started once at boot ([`spawn_backup_loop`](../../src-tauri/crates/app/src/backup.rs)). When disabled, parks on a `tokio::sync::Notify` (zero cost) until the user toggles. When enabled, computes the next deadline as `last_run_at + interval_days * 86_400_000` and uses `tokio::select!` between a sleep and the same `Notify` so config changes wake it without waiting for the old sleep to expire.
- **Pass** ([`run_one_backup`](../../src-tauri/crates/app/src/backup.rs)) iterates every row in `profile`, calls the shared [`profile_io::write_archive`](../../src-tauri/crates/app/src/commands/profile_io.rs) (pub-crate-ified from the manual-export path so the two stay bit-compatible), and applies retention per profile (`<sanitized-name>-*.waveflow` sorted by mtime, oldest beyond `retention` deleted). The active profile gets a `PRAGMA wal_checkpoint(TRUNCATE)` first; inactive profiles are already cold on disk (the pool ran a checkpoint at switch / shutdown).
- **Failure isolation:** per-profile errors are logged but don't abort the pass — one corrupt profile shouldn't block backups of the healthy ones.
- **Commands** in [`commands/backup.rs`](../../src-tauri/crates/app/src/commands/backup.rs): `get_backup_config`, `set_backup_config` (also signals the loop), `run_backup_now`. UI is [`BackupCard`](../../src/components/views/settings/BackupCard.tsx) in Settings → Storage and backups → Profile backups right after the manual export/import.

## Settings categories

The ten categories, what each one houses and the settings search are described under [Settings navigation](#settings-navigation).

### App preferences

[`commands/preferences.rs`](../../src-tauri/crates/app/src/commands/preferences.rs) owns the three toggles that have a side effect outside the database — the original release shipped the switches without wiring them, so they reset on every restart:

- **Minimize to tray** — `app_setting['app.minimize_to_tray']`, process-wide, default **ON**. Mirrored onto an atomic in `PreferencesState` so the `WindowEvent::CloseRequested` handler in `lib.rs` is a single load. When OFF, closing the window arms the `QuitGate` and runs the normal shutdown path.
- **Scan on start** — `profile_setting['library.scan_on_start']`, per profile, default **OFF**. Consulted once at the end of `AppState::init`, so the rescan starts before the frontend has queried the library and the app appears to have noticed the new files on its own.
- **Auto start** — delegated to [`tauri-plugin-autostart`](https://v2.tauri.app/plugin/autostart/), which writes the OS-level entry (registry key / LaunchAgent / xdg autostart `.desktop`). The commands are thin wrappers so the frontend keeps one vocabulary instead of mixing plugin calls with our own.

### Persistent zoom

[`useUiZoom`](../../src/hooks/useUiZoom.ts), mounted once in `AppLayout`, hydrates the saved level and applies it through Tauri's `setZoom` — the **WebView** scales natively, so text stays crisp (this is not a CSS `transform: scale`). Range 0.5×–2.0× in 0.1 steps.

`Ctrl+=` / `Ctrl+-` / `Ctrl+0` follow the VS Code / browser convention and are bound at the window level so they work from any view (skipped while an input / textarea / contenteditable has focus). The handler reads a **ref**, not the state value, so rapid keystrokes accumulate before React flushes a render, and rolls it back if the apply fails. The Settings slider and the shortcut stay in sync through the `waveflow:ui-zoom-changed` window event.

### Embedded changelog

The About view renders the release history without a network call: [`build.rs`](../../src-tauri/crates/app/build.rs) parses `git log` at **compile time** and embeds the result, so the shipped binary carries its own changelog and offline installs still show it.

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

[`OnboardingModal`](../../src/components/common/OnboardingModal.tsx) walks new profiles through a multi-step wizard. Steps in order:

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
