# Plugins

WaveFlow ships a WebAssembly plugin system ([RFC-002](../rfcs/RFC-002-plugin-sdk.md)) so third-party code can add **sources** (new places to play from), **metadata** (extra artwork / info), or UI without touching the signed core. Plugins are sandboxed WASM components, loaded at runtime, and installed from a curated in-app store. The core repo carries **no** plugin code — the store catalogue and each plugin live in their own repositories.

## Runtime & sandbox

The host is [`waveflow_core::plugin::runtime`](../../src-tauri/crates/core/src/plugin/runtime.rs) — a wasmtime + WASI-p2 host that loads WASM **components** at runtime from two roots:

- `<app-data>/waveflow/plugins/` — the writable **sideload** root (where the store installs).
- `<resource>/plugins/` — installer-bundled plugins, re-seeded into the sideload root at boot.

A plugin declares a **world** (`source` or `metadata`, e.g. `waveflow:metadata@1.1.0`) and, in its `manifest.toml`, the host capabilities it needs. Every capability is **permission-gated**: outbound HTTP goes through the host's allowlisted `waveflow:host/http` (a plugin can only reach the hosts its manifest lists — surfaced in the UI as the "Can reach:" chip), and persistence is limited to the plugin's own **scratch store** (`waveflow:host/storage`, a small per-plugin quota — the "User storage" chip). Plugins have **no filesystem access**. The host also **serialises** calls into a given plugin, so a plugin can't storm an upstream API.

Host imports currently exposed to guests: `http` (permissioned fetch), `storage` (scratch read/write state), `log`, and `config` (read-only access to the user's plugin options — see below). The `metadata` world reuses `source`'s `waveflow:host/*` types via bindgen `with:`, so there is one set of host implementations behind both.

## The plugin store

The install path is [`commands/plugin_store.rs`](../../src-tauri/crates/app/src/commands/plugin_store.rs). It fetches a curated catalogue from a **source cascade** (first that answers wins, all identical):

1. `waveflow.app/api/plugins/registry`
2. `raw.githubusercontent.com/InstaZDLL/waveflow-plugins/main/registry.json`
3. jsDelivr mirror of the same file

Installing an entry downloads that plugin's **pinned GitHub release**, then:

- **verifies `plugin.wasm`'s blake3 against the registry entry** — the *registry* is the trusted pin, not the release, so a compromised release fails the hash and is rejected;
- sanity-checks the manifest (`id` / `version` / `world`);
- **stage-swaps** the verified artifact into the sideload root (so a failed download never corrupts an installed plugin).

Every registry fetch honours [`offline::is_offline()`](../../src-tauri/crates/app/src/offline.rs) and short-circuits when offline. Because the catalogue and each plugin live in **separate repos** ([`InstaZDLL/waveflow-plugins`](https://github.com/InstaZDLL/waveflow-plugins) + per-plugin repos), a grey-area plugin carries no liability for the signed core and a takedown is one registry commit.

UI: [`PluginStoreCard`](../../src/components/views/settings/PluginStoreCard.tsx) sits above [`PluginsCard`](../../src/components/views/settings/PluginsCard.tsx) under **Settings → Plugins**; i18n keys under `settings.pluginStore.*`.

## Per-plugin options

A plugin can declare `[[options]]` in its `manifest.toml` (`key` / `type` = `bool` · `enum` · `text` / `label` / `default` / `choices?` / `description?`, parsed + validated in [`manifest.rs`](../../src-tauri/crates/core/src/plugin/manifest.rs)). The user edits them in a per-plugin **⚙️ options panel** ([`PluginOptions`](../../src/components/views/settings/PluginOptions.tsx)) revealed inline under the plugin's row, via `get_plugin_options` / `set_plugin_option` ([`commands/plugins.rs`](../../src-tauri/crates/app/src/commands/plugins.rs)).

Values persist in `<state_dir>/.plugin-config.json` ([`plugin_config`](../../src-tauri/crates/core/src/plugin/plugin_config.rs)) — the single source of truth, with no `app_setting` row and excluded from the scratch quota. They reach the guest through the read-only `waveflow:host/config.get-option` import, pinned at instantiate time. The import is additive: a plugin built before it still instantiates.

> **Note:** plugin **names, descriptions and option labels** come straight from each plugin's `manifest.toml` (and store descriptions from `registry.json`), so they are English-only today — they live in the plugin repos, outside the app's i18next files. Localizing them is tracked in [#440](https://github.com/InstaZDLL/WaveFlow/issues/440).

## Official plugins

### Web Radio (`source` world)

Internet radio backed by [radio-browser.info](https://www.radio-browser.info) — 30 000+ stations searchable by country, language, tag, or codec. Live streams are routed through the cpal engine, with **live ICY "now playing"** song titles de-interleaved from the stream, per-profile station **favorites**, and **country browsing** (local-station shortcut + 200+ country picker).

The plugin queries radio-browser live and can't host SQLite, so the **offline catalogue** is a native side-path ([`commands/web_radio_catalogue.rs`](../../src-tauri/crates/app/src/commands/web_radio_catalogue.rs)): `download_radio_catalogue` snapshots the ~35k-station directory into an `app.db` `radio_station` table + contentless FTS5 index (user-triggered from Settings → Data), and `resolve_radio_catalogue` answers the **same** opaque query tokens (`top` / `tag:x` / `country:xx` / free text) returning the **same** `PluginTrack` shape. [`WebRadioView`](../../src/components/views/WebRadioView.tsx) routes browse/search through it when offline mode is on, or when `radio.catalogue.local_first` is enabled with a catalogue present. The stream URL rides inside the track id (`url:<stream>`), so **browsing and resolving a station never touch radio-browser**. Playback itself still streams from the remote station, so it always needs network — offline mode removes the catalogue/API dependency, not the stream.

### Apple Motion Artwork (`metadata` world)

Animated album covers (motion artwork) from Apple Music, rendered behind the now-playing view. It resolves an album to a directly-playable looping **mp4** URL (the desktop webview has no HLS.js), caching each result — a positive hit **and** a confirmed-miss sentinel — in its 10 MB scratch store, so **once an album has resolved to a hit or a confirmed miss it never hits Apple again**. Transient failures (network errors) are deliberately **not** cached, so a blip never permanently marks an album as "no motion" — that album is simply retried on the next lookup.

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
