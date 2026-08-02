# Plugins

WaveFlow ships a WebAssembly plugin system ([RFC-002](../rfcs/RFC-002-plugin-sdk.md)) so third-party code can add **sources** (new places to play from), **metadata** (extra artwork / info), or UI without touching the signed core. Plugins are sandboxed WASM components, loaded at runtime, and installed from a curated in-app store. The core repo carries **no** plugin code — the store catalogue and each plugin live in their own repositories.

## Runtime & sandbox

The host is [`waveflow_core::plugin::runtime`](../../src-tauri/crates/core/src/plugin/runtime.rs) — a wasmtime + WASI-p2 host that loads WASM **components** at runtime from two roots:

- `<app-data>/waveflow/plugins/` — the writable **sideload** root (where the store installs).
- `<resource>/plugins/` — installer-bundled plugins, re-seeded into the sideload root at boot.

A plugin declares a **world** (`source`, `metadata`, or `ui` — e.g. `waveflow:metadata@1.1.0`) and, in its `manifest.toml`, the host capabilities it needs. Every capability is **permission-gated**: outbound HTTP goes through the host's allowlisted `waveflow:host/http` (a plugin can only reach the hosts its manifest lists — surfaced in the UI as the "Can reach:" chip), and persistence is limited to the plugin's own **scratch store** (`waveflow:host/storage`, a small per-plugin quota — the "User storage" chip). Plugins have **no filesystem access**. The host also **serialises** calls into a given plugin, so its host operations run one at a time rather than concurrently — this bounds concurrency, not overall request volume, and the host does not itself rate-limit or back off (a plugin stays polite by caching its results, as the official ones do).

Host imports currently exposed to guests: `http` (permissioned fetch), `storage` (scratch read/write state), `log`, `config` (read-only access to the user's plugin options — see below), and `library` (the `ui` world's redacted artist read — see [The UI world](#the-ui-world-waveflowuiv1)). The `metadata` **and** `ui` worlds reuse `source`'s `waveflow:host/*` types via bindgen `with:`, so there is one set of host implementations behind all three — the `ui` world adds only the fresh `library` import on top.

## The plugin store

The install path is [`commands/plugin_store.rs`](../../src-tauri/crates/app/src/commands/plugin_store.rs). It fetches a curated catalogue from a **source cascade** (first that answers wins, all identical):

1. `waveflow.app/api/plugins/registry`
2. `raw.githubusercontent.com/InstaZDLL/waveflow-plugins/main/registry.json`
3. jsDelivr mirror of the same file

Installing an entry downloads that plugin's **pinned GitHub release**, then:

- **verifies `plugin.wasm`'s blake3 against the registry entry** — the _registry_ is the trusted pin, not the release, so a compromised release fails the hash and is rejected;
- sanity-checks the manifest (`id` / `version` / `world`);
- **stage-swaps** the verified artifact into the sideload root (so a failed download never corrupts an installed plugin).

Every registry fetch honours [`offline::is_offline()`](../../src-tauri/crates/app/src/offline.rs) and short-circuits when offline. Because the catalogue and each plugin live in **separate repos** ([`InstaZDLL/waveflow-plugins`](https://github.com/InstaZDLL/waveflow-plugins) + per-plugin repos), a grey-area plugin carries no liability for the signed core and a takedown is one registry commit.

UI: [`PluginStoreCard`](../../src/components/views/settings/PluginStoreCard.tsx) sits above [`PluginsCard`](../../src/components/views/settings/PluginsCard.tsx) under **Settings → Plugins**; i18n keys under `settings.pluginStore.*`.

## Per-plugin options

A plugin can declare `[[options]]` in its `manifest.toml` (`key` / `type` = `bool` · `enum` · `text` / `label` / `default` / `choices?` / `description?`, parsed + validated in [`manifest.rs`](../../src-tauri/crates/core/src/plugin/manifest.rs)). The user edits them in a per-plugin **⚙️ options panel** ([`PluginOptions`](../../src/components/views/settings/PluginOptions.tsx)) revealed inline under the plugin's row, via `get_plugin_options` / `set_plugin_option` ([`commands/plugins.rs`](../../src-tauri/crates/app/src/commands/plugins.rs)).

Values persist in `<state_dir>/.plugin-config.json` ([`plugin_config`](../../src-tauri/crates/core/src/plugin/plugin_config.rs)) — the single source of truth, with no `app_setting` row and excluded from the scratch quota. They reach the guest through the read-only `waveflow:host/config.get-option` import, pinned at instantiate time. The import is additive: a plugin built before it still instantiates.

## Localized manifest strings

Plugin descriptions and option labels are authored in each plugin's `manifest.toml` (store descriptions in `registry.json`), outside the app's i18next files — so `t()` can never reach them. Instead the **format itself carries the translations**, for `plugin.description`, each option's `label` / `description`, and a registry entry's `description`.

### The publishable form: `*_i18n` siblings

**This is the one to use for anything users install.** Keep the plain string exactly where it is and add a sibling map next to it:

```toml
description = "Animated album covers from Apple Music."

[plugin.description_i18n]
fr = "Pochettes animées depuis Apple Music."
de = "Animierte Albumcover aus Apple Music."

[[options]]
key = "prefer_hevc"
type = "bool"
label = "Prefer 4K HEVC covers"

[options.label_i18n]
fr = "Préférer les pochettes 4K HEVC"
```

```jsonc
// registry.json — same idea
"description": "Animated album covers from Apple Music.",
"description_i18n": { "fr": "Pochettes animées depuis Apple Music." }
```

An older WaveFlow ignores the field it doesn't know and renders the plain English string; a current one folds the two into one value at parse time ([`merge_localized_siblings`](../../src-tauri/crates/core/src/plugin/manifest.rs)), the plain string taking the `en` slot. **No `min_app_version` bump, no broken store, nobody loses access to the plugin.**

### The inline form, and why it is not publishable

`plugin.description` / `label` / `description` also accept a `{ lang -> text }` table directly ([`LocalizedString`](../../src-tauri/crates/core/src/plugin/manifest.rs)):

```toml
[plugin.description]
en = "Animated album covers from Apple Music."
fr = "Pochettes animées depuis Apple Music."
```

It reads better, and it is fine for a manifest that never reaches an older host. But a WaveFlow predating this feature expects a string and hard-errors on the table:

- in a **manifest**, the plugin is dropped wholesale (unreadable manifest) — recoverable only by raising the registry entry's `min_app_version`, which also cuts those users off from the version they can still run;
- in **`registry.json`**, which is a single document every installed version fetches, the catalogue fails to decode from all three sources and **the store goes dark on that build**. `min_app_version` cannot save it: it is read after the decode that already failed.

So: inline for local experiments, `*_i18n` for anything published.

### Resolution

Key on the app's canonical locale codes (the 17 in [`src/i18n/index.ts`](../../src/i18n/index.ts)); brand tokens (`WaveFlow`, `Apple Music`, `Last.fm`, `HEVC`…) stay verbatim in every language.

The host hands the merged value through untouched and the UI resolves it against the active i18next language via [`useLocalizedText`](../../src/hooks/useLocalizedText.ts), so a language switch re-renders instantly with no backend round-trip. The fallback chain — exact code → base language (`pt-BR` → `pt`) → `en` → any entry — is implemented twice, in [`LocalizedString::resolve`](../../src-tauri/crates/core/src/plugin/manifest.rs) and [`resolveLocalizedText`](../../src/lib/localizedText.ts); **change them together**.

Blank entries are skipped at every step instead of counting as a hit, so `fr = ""` next to an English string renders the English — an empty slot is an authoring accident, and letting it win would blank a store card or leave an option control with no accessible name (the UI substitutes the option key only on a `None`). A localized field that ends up declaring zero languages is refused outright at parse time.

## The UI world (`waveflow:ui/v1`)

A `ui`-world plugin renders its own view inside WaveFlow **without shipping any React**. The security boundary is a **JSON view descriptor**: the guest describes a declarative tree (title, sections, item cards, images, action buttons) and the host draws it with WaveFlow-native components. A plugin never injects HTML, CSS, JavaScript, or React code — a hostile descriptor can only ask for widgets the host already knows how to render, so it can't run code in the app's origin.

The exported interface `extension` is three functions ([`wit/ui/plugin.wit`](../../src-tauri/crates/plugin-sdk/wit/ui/plugin.wit)):

- **`manifest() -> mount-point`** — sidebar registration (label + optional lucide icon name + initial path). The host reads it once to place a navigable entry; the plugin doesn't draw the sidebar itself.
- **`render(path) -> result<string, string>`** — returns the current view as a JSON descriptor string for an internal `path`.
- **`on-event(event, payload) -> result<string, string>`** — a user action (a descriptor `event` button's opaque `event` + `payload`) round-trips here, and the plugin returns the **next full descriptor** that replaces the view. There is no diff/patch protocol — every action re-renders. An `open-url` action, by contrast, is handled entirely host-side and never reaches the guest.

The host side is thin: [`bindings::ui`](../../src-tauri/crates/core/src/plugin/bindings.rs) binds the world (reusing `source`'s host types via `with:`), [`runtime::{ui_manifest, ui_render, ui_event}`](../../src-tauri/crates/core/src/plugin/runtime.rs) instantiate + call it, and the Tauri commands [`list_ui_plugins` / `plugin_ui_render` / `plugin_ui_event`](../../src-tauri/crates/app/src/commands/plugins.rs) drive the frontend. Sidebar entries + routing are built dynamically off `manifest()` (keyed on plugin id), not hardcoded per plugin — a plugin whose `manifest()` traps is skipped + logged rather than blanking the nav.

### The redacted `library.read_artists` capability

A UI plugin often needs to know which artists the user follows (Release Radar keys new releases off them). The `library` host import ([`wit/ui/deps/host/host.wit`](../../src-tauri/crates/plugin-sdk/wit/ui/deps/host/host.wit)) grants a **redacted** read only:

```wit
list-artists: func(limit: u32) -> result<list<artist>, string>;
// artist = { id: u64, name: string, track-count: u32 }
```

Names + aggregate track counts + an opaque id — **no file paths, no per-track rows, no raw DB access**. It's permission-gated (`library.read_artists` in the manifest, surfaced as its own permission chip), doubly clamped (the host loads ≤ [`MAX_LIBRARY_ARTISTS`](../../src-tauri/crates/core/src/plugin/host_impl.rs) and re-caps the guest's `limit`), and **snapshot-injected**: the host queries the active profile on the async side and hands the guest a ready list, so the guest never touches SQLite. Because it reads local state only, it works offline. A plugin without the permission gets `Err("permission denied: library.read_artists")` even if the snapshot is present — the redaction is enforced host-side, not left to guest good behaviour (proven both ways in [`tests/plugin_ui.rs`](../../src-tauri/crates/core/tests/plugin_ui.rs) against the `ui-fixture` component).

The first `ui`-world consumer is **Release Radar** (issue #443), published at [`InstaZDLL/waveflow-plugin-release-radar`](https://github.com/InstaZDLL/waveflow-plugin-release-radar) and listed in the registry, which — like every plugin — lives in its own repo, not the signed core. The core carries only the world surface + a test-only `ui-fixture` under [`plugins/ui-fixture/`](../../src-tauri/plugins/ui-fixture/) (never bundled, never shipped).

## The Canvas world (`waveflow:canvas/v1`)

A `canvas`-world plugin resolves a **per-track Canvas** — a short looping video the host renders behind the now-playing view (issue #473), Spotify-Canvas style. It is distinct from a `metadata` plugin's per-album `motion-cover-url`: Canvas is keyed per **track** and sits **above** motion artwork in the backdrop precedence (**manual Canvas > plugin Canvas > motion > slideshow > static cover**). The world is deliberately generic — a provider can source Canvases from anywhere.

The exported interface `provider` is one function ([`wit/canvas/plugin.wit`](../../src-tauri/crates/plugin-sdk/wit/canvas/plugin.wit)):

```wit
track-canvas: func(artist: string, title: string, album: option<string>, duration-ms: option<u32>)
  -> result<option<canvas>, string>;
// canvas = { url: string, entity-id: option<string> }
```

`url` MUST be a directly-playable progressive `.mp4` (the webview `<video>` has no HLS.js, so an HLS source is resolved to an mp4 by the plugin first). `ok(none)` = no Canvas for this track; `err` = a provider failure. The host imports are the standard four (`http`/`log`/`storage`/`config`) — **no** `library` read.

The host side mirrors the metadata fanout: [`bindings::canvas`](../../src-tauri/crates/core/src/plugin/bindings.rs) binds the world (reusing `source`'s host types via `with:`), [`runtime::canvas_track_canvas`](../../src-tauri/crates/core/src/plugin/runtime.rs) instantiates + calls it, and the Tauri command [`fetch_track_canvas`](../../src-tauri/crates/app/src/commands/canvas.rs) fans out to every enabled `canvas` plugin (per-plugin lock + blocking task + 20 s timeout), returning the **first safe hit** (an SSRF guard rejects a non-https / loopback URL before it reaches the webview). It is **fail-soft**: a plugin error, panic, or timeout is logged and skipped, never surfaced — so a misbehaving Canvas provider can never break playback; the frontend simply falls back down the precedence chain. Frontend: [`useTrackCanvas`](../../src/hooks/useTrackCanvas.ts) tries the manual local Canvas first, then this command, and [`CanvasStage`](../../src/components/player/CanvasStage.tsx) tells a local path from a remote URL (same `http(s)` split as `MotionCoverOverlay`).

The core carries only the world surface + a test-only `canvas-fixture` under [`plugins/canvas-fixture/`](../../src-tauri/plugins/canvas-fixture/) (never bundled), exercised by [`tests/plugin_canvas.rs`](../../src-tauri/crates/core/tests/plugin_canvas.rs). The first consumer — a Spotify Canvas plugin — lives in its own separate, **unsigned** repo, never the core (the Spotify path is a grey-area of their Developer Terms, isolated exactly like the excluded YouTube path).

## Official plugins

### Web Radio (`source` world)

Internet radio backed by [radio-browser.info](https://www.radio-browser.info) — 30 000+ stations searchable by country, language, tag, or codec. Live streams are routed through the cpal engine, with **live ICY "now playing"** song titles de-interleaved from the stream, per-profile station **favorites**, and **country browsing** (local-station shortcut + 200+ country picker).

The plugin queries radio-browser live and can't host SQLite, so the **offline catalogue** is a native side-path ([`commands/web_radio_catalogue.rs`](../../src-tauri/crates/app/src/commands/web_radio_catalogue.rs)): `download_radio_catalogue` snapshots the ~35k-station directory into an `app.db` `radio_station` table + contentless FTS5 index (user-triggered from Settings → Data), and `resolve_radio_catalogue` answers the **same** opaque query tokens (`top` / `tag:x` / `country:xx` / free text) returning the **same** `PluginTrack` shape. [`WebRadioView`](../../src/components/views/WebRadioView.tsx) routes browse/search through it when offline mode is on, or when `radio.catalogue.local_first` is enabled with a catalogue present. The stream URL rides inside the track id (`url:<stream>`), so **browsing and resolving a station never touch radio-browser**. Playback itself still streams from the remote station, so it always needs network — offline mode removes the catalogue/API dependency, not the stream.

### Apple Motion Artwork (`metadata` world)

Animated album covers (motion artwork) from Apple Music, rendered behind the now-playing view. It resolves an album to a directly-playable looping **mp4** URL (the desktop webview has no HLS.js), caching each result — a positive hit **and** a confirmed-miss sentinel — in its 10 MB scratch store, so **once an album has resolved to a hit or a confirmed miss it never hits Apple again**. Transient failures (network errors) are deliberately **not** cached, so a blip never permanently marks an album as "no motion" — that album is simply retried on the next lookup.

### Release Radar (`ui` world)

A discovery view of recent releases from the artists in your library — the first `ui`-world plugin. It reads the redacted artist list (`library.read_artists`), searches [MusicBrainz](https://musicbrainz.org) for each artist's release-groups from the last ~6 months, and renders them as a native view descriptor with [Cover Art Archive](https://coverartarchive.org) covers and outbound MusicBrainz links — **no playback, no YouTube**. The scan is **incremental**: a bounded batch of artists per click, requests spaced to respect MusicBrainz's ≤ 1 req/s guidance and backing off cleanly on a `503`/`429` (never hammering a rate-limit response), with results + a resume cursor cached in its scratch store so re-opening the view is instant. Listed in the registry at [`InstaZDLL/waveflow-plugin-release-radar`](https://github.com/InstaZDLL/waveflow-plugin-release-radar); needs WaveFlow 1.8.0+ (the `ui` world).

## Motion artwork pipeline

[`commands/motion_artwork.rs::fetch_album_motion_artwork`](../../src-tauri/crates/app/src/commands/motion_artwork.rs) fans an `album-info` request out to every enabled `metadata`-world plugin; the first `motion-cover-url` wins. The result renders as a muted-loop `<video>` overlay ([`MotionCoverOverlay`](../../src/components/player/MotionCoverOverlay.tsx) + [`useAlbumMotionArtwork`](../../src/hooks/useAlbumMotionArtwork.ts)) over the **static** cover in ImmersiveNowPlaying + NowPlayingPanel — it is purely additive: no motion (or a dead URL that 404s) simply falls back to the static album cover, which the motion path never touches.

- **Manual motion cover** (issue #408): a user can set a local mp4 per album via the "Set motion cover" button (Film icon) on the album page → [`MotionCoverPickerModal`](../../src/components/common/MotionCoverPickerModal.tsx). It stores into a never-evicted per-profile `motion/` dir (64 MiB cap) and takes precedence over any plugin resolution.
- **Opt-in local cache** (default **OFF**, `app_setting['motion_artwork.cache_enabled']`): when on, the resolved plugin mp4 is downloaded into an app-wide LRU cache (`<app-data>/waveflow/motion_cache/`, hash-addressed by source URL, 1 GB cap, mtime-based eviction) and served from disk. OFF relies on the webview's transient HTTP cache. Toggle + size + clear live in the same ⚙️ panel (`PluginOptions`); i18n under `settings.motionArtwork.*`.

## Security model at a glance

- **Sandboxed** — WASM component in wasmtime, no ambient authority.
- **Permission-gated** — HTTP is allowlisted per manifest; storage is a bounded per-plugin scratch quota; no filesystem.
- **Verified installs** — `plugin.wasm` blake3 is pinned by the trusted registry, checked before a stage-swap; a tampered release fails.
- **Offline-aware** — every registry / plugin fetch respects process-wide offline mode.
- **Isolated liability** — plugin code lives in separate repos; nothing grey-area ships inside the signed core.
