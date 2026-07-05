# RFC-002 — Plugin SDK: WASM-component plugins for sources, metadata, and UI

- **Status**: Draft
- **Date**: 2026-05-29
- **Authors**: @InstaZDLL
- **Supersedes**: —
- **Depends on**: [RFC-001](RFC-001-waveflow-server.md) — extracted `waveflow-core` is the surface plugins call into; the parity goal here (desktop ≡ server) is what RFC-001 enabled.
- **Implementation tracking**: opened as the `Phase 3 — Plugin SDK` GitHub milestone once 1.b lands.

---

> **Status update (2026-07): plugin store shipped.** §2's "sideload-first
> distribution" and the §3 non-goal "a plugin marketplace / a registry is
> a Phase 4+ conversation" are **superseded**. A curated store now ships:
> a git-versioned registry repo ([`InstaZDLL/waveflow-plugins`](https://github.com/InstaZDLL/waveflow-plugins))
> holds a `registry.json` catalogue; the desktop app fetches it through an
> ordered source cascade (`waveflow.app/api/plugins/registry` →
> `raw.githubusercontent` → jsDelivr) and installs a plugin by downloading
> its pinned release, **verifying the `plugin.wasm` blake3 against the
> registry entry** (the registry, not the release, is the trusted pin),
> and stage-swapping it into the writable sideload root for the runtime to
> load. Sideload-by-hand still works; the store is the ergonomic path on
> top. See [`commands/plugin_store.rs`](../../src-tauri/crates/app/src/commands/plugin_store.rs)
> + the `settings.pluginStore` UI. Everything else in this RFC (WASM
> Component Model, WIT worlds, sandbox, permission enforcement) stands.

---

## 1. Context

Phase 1 (RFC-001) splits WaveFlow into `waveflow-core` + `waveflow` (Tauri app) and prepares `waveflow-server` to consume the same business logic. With that infrastructure in place, the next bottleneck is **what we ship as core**: Web Radio (#171), alternative metadata providers (MusicBrainz, Discogs, Genius), specialized views for niche music collections (DJ sets, classical, audiobooks) are all features users will ask for, and every one of them in core means lock-in to one vendor's API quirks plus an explosion of the maintenance surface.

A plugin system flips that constraint. WaveFlow core ships the engine + the obvious vendors (Deezer / Last.fm / LRCLIB are kept in core because they're already there); everything else lives behind an SDK and can be authored, distributed, and updated independently.

This RFC locks in the plugin architecture **before any plugin code lands** so the very first plugin (Web Radio in v1.5.0) doesn't paint us into a corner.

## 2. Goals

- **Three plugin types** as v1 targets: source providers, metadata enrichers, UI extensions. Each has a well-defined WIT interface and a small host API surface.
- **Sandboxed by default.** A buggy plugin must not crash the host, leak memory across plugin boundaries, or escalate beyond the permissions its manifest declares.
- **Desktop ↔ server parity from day one.** The same `.wasm` artifact runs on the Tauri app, on `waveflow-server` (RFC-001 Phase 1.b+), and eventually on the mobile shell (Phase 4). No fork.
- **Authoring in any language that targets WASM Component Model** — Rust is canonical (we publish a `waveflow-plugin` crate), Go / AssemblyScript / Python via componentize-py also work.
- **Sideload-first distribution.** A user drops a `.wasm` + `manifest.toml` in a plugins directory, restarts, and it shows up. A curated registry is explicitly out of v1 scope.

## 3. Non-goals

- **Audio DSP plugins.** The cpal callback is lock-free and allocation-free; allowing arbitrary WASM in that path is a real-time safety footgun this RFC won't open. Custom EQ presets / DSP effects stay a core-side feature for the foreseeable future.
- **A plugin marketplace** with reviews, ratings, install counts. v1 is sideload-only. A registry is a Phase 4+ conversation.
- **Hot reload of running plugins.** Plugins are loaded at app start; reloading requires a restart. Acceptable since the use case is "I just installed it", not "I'm authoring it" (authors run the dev harness, see §6.7).
- **Cross-plugin communication.** Plugins talk to the host only — never directly to each other. Removes a whole class of dependency-graph and supply-chain issues.
- **JavaScript / TypeScript plugins running in a JS sandbox.** WASM Component Model is the single runtime. JS authors can target it via `jco` / `componentize-js` but the runtime stays WASM.

## 4. Architecture overview

```bash
                                ┌─────────────────────────────────────────────┐
                                │  ~/.config/waveflow/plugins/                │
                                │    webradio/                                │
                                │      manifest.toml                          │
                                │      plugin.wasm                            │
                                │    musicbrainz/                             │
                                │      manifest.toml                          │
                                │      plugin.wasm                            │
                                └────────────────────┬────────────────────────┘
                                                     │ scan + load on boot
                                                     ▼
┌────────────────────────────────────────────────────────────────────────────┐
│  waveflow-core::plugin                                                     │
│                                                                            │
│  ┌──────────────┐   ┌────────────────────┐   ┌────────────────────────┐    │
│  │ PluginHost   │←→ │ wasmtime runtime   │←→ │ WIT-bindgen guest exports
│  │  - registry  │   │  - one Store per   │   │   waveflow:source/v1/  │    │
│  │  - manifests │   │    plugin instance │   │   waveflow:metadata/v1/│    │
│  │  - lifecycle │   │  - per-plugin fuel │   │   waveflow:ui/v1/      │    │
│  └──────┬───────┘   └────────────────────┘   └────────────────────────┘    │
│         │                                                                  │
│         │ trait calls                                                      │
│         ▼                                                                  │
│  ┌──────────────────┐    ┌──────────────────┐    ┌──────────────────────┐  │
│  │ SourceProvider   │    │ MetadataEnricher │    │ UiExtension          │  │
│  │ trait            │    │ trait            │    │ trait                │  │
│  │  - resolve()     │    │  - artist_info() │    │  - render() → View   │  │
│  │  - list()        │    │  - album_info()  │    │  - on_event()        │  │
│  │  - stream_url()  │    │  - lyrics()      │    │                      │  │
│  └──────────────────┘    └──────────────────┘    └──────────────────────┘  │
│         ▲                         ▲                         ▲              │
└─────────┼─────────────────────────┼─────────────────────────┼──────────────┘
          │                         │                         │
   tauri::command           Library + scanner            React renderer
   (player.rs)              enrichment pipeline          (desktop) /
                                                         JSX renderer (web)
```

`waveflow-core` owns the entire plugin stack — `wasmtime` runtime, WIT bindings, the three plugin-type traits, the manifest loader. The Tauri app calls into `PluginHost` as a regular `waveflow-core` API; `waveflow-server` (RFC-001 Phase 1.b+) does the same. Same plugins, same code path.

## 5. Repositories

No new top-level repo for v1. Everything lands inside the existing two:

| Repo               | Path                                | Purpose                                                                                            |
| ------------------ | ----------------------------------- | -------------------------------------------------------------------------------------------------- |
| `waveflow`         | `src-tauri/crates/core/src/plugin/` | `PluginHost`, runtime, WIT bindings, type-level traits. Lives in core so the server consumes them. |
| `waveflow`         | `src-tauri/crates/plugin-sdk/`      | Rust crate published to crates.io as `waveflow-plugin`. WIT files + guest macros + dev harness.    |
| _(authors' repos)_ | independent                         | Each plugin author owns their repo. We provide a `cargo generate` template + CI examples.          |

The WIT files (`.wit`) are the public contract. They are duplicated nowhere — both core and SDK consume the same files from `crates/plugin-sdk/wit/`.

A dedicated `waveflow-plugins` repo for first-party plugins (Web Radio, MusicBrainz) is created when the **second** plugin lands, not the first. Until then they live next to their author in standalone repos to avoid premature monorepo overhead.

## 6. Decisions

### 6.1 Runtime: wasmtime + Component Model

**Decision: `wasmtime` with the [Component Model](https://component-model.bytecodealliance.org/) and WIT-based interfaces.** Rejected `wasmer`, `wasmi`, `wasmtime-core` (pre-component).

| Option                          | Rejected because                                                                                                                                                                  |
| ------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `wasmer`                        | Smaller ecosystem, slower Component Model adoption, licensing concerns (MIT vs Apache mix).                                                                                       |
| `wasmi`                         | Interpreter only — no JIT. Fine for tiny plugins but DSP / large parsers will be too slow. Loses cross-platform deterministic-execution benefit that doesn't matter for our use.  |
| `wasmtime` core (no components) | Workable but means inventing our own ABI for cross-language compat. Component Model is the whole point — it's an industry-standard ABI; we get Go / Python / JS plugins for free. |
| `wasmtime` + components         | ✅ Chosen. Largest ecosystem, WIT toolchain is mature (`wit-bindgen` 0.40+), `componentize-js` lands TS support, WASI 0.2 = stable host APIs.                                     |

Wasmtime version locked at the major version current at v1.5.0 cut (likely `wasmtime` 30.x). Component Model is post-1.0 and stable.

Tradeoff accepted: binary size impact (~12 MB added to the bundle for the wasmtime crate). The desktop installer already ships at ~75 MB; this is noise compared to the gains.

### 6.2 Plugin types: three WIT worlds

WIT files live at `src-tauri/crates/plugin-sdk/wit/`. Three worlds, one per plugin type:

```wit
// waveflow:source/v1 — provides tracks the engine can play
package waveflow:source@1.0.0;

interface provider {
  record track {
    id: string,
    title: string,
    artist: string,
    album: option<string>,
    duration-ms: u32,
    artwork-url: option<string>,
  }

  /// List entry points the plugin offers (categories, stations, charts).
  list-entries: func() -> result<list<string>, string>;
  /// Resolve a category / search query to a track list.
  resolve: func(query: string) -> result<list<track>, string>;
  /// Return the playable URL for a track. Called lazily at play time so
  /// short-lived tokens stay fresh. Plugins that serve direct streams
  /// can return the same URL on every call.
  stream-url: func(track-id: string) -> result<string, string>;
}

world plugin {
  import waveflow:host/http@1.0.0;
  import waveflow:host/log@1.0.0;
  import waveflow:host/storage@1.0.0;
  export provider;
}
```

```wit
// waveflow:metadata/v1 — biographies, similar artists, lyrics
package waveflow:metadata@1.0.0;

interface enricher {
  record artist-info {
    bio: option<string>,
    image-url: option<string>,
    similar: list<string>,
  }
  record album-info {
    description: option<string>,
    cover-url: option<string>,
    track-count: option<u32>,
  }

  artist-info: func(name: string) -> result<artist-info, string>;
  album-info: func(artist: string, title: string) -> result<album-info, string>;
  lyrics: func(artist: string, title: string) -> result<option<string>, string>;
}

world plugin {
  import waveflow:host/http@1.0.0;
  import waveflow:host/log@1.0.0;
  import waveflow:host/storage@1.0.0;
  export enricher;
}
```

```wit
// waveflow:ui/v1 — view descriptors the host renders
package waveflow:ui@1.0.0;

interface extension {
  variant view {
    list(list-view),
    grid(grid-view),
    detail(detail-view),
  }
  record list-view {
    title: string,
    rows: list<row>,
  }
  record row {
    primary: string,
    secondary: option<string>,
    artwork-url: option<string>,
    action: option<action>,
  }
  variant action {
    play(string),         // track-id to send to the engine
    navigate(string),     // plugin-internal path
    open-url(string),     // open in system browser
  }

  /// Where the plugin asks to be mounted in the host navigation.
  record mount-point {
    sidebar-label: string,
    sidebar-icon: option<string>,  // lucide-react icon name
    initial-path: string,
  }
  manifest: func() -> mount-point;

  /// Render the view for the current internal path.
  render: func(path: string) -> result<view, string>;
  /// React to an action triggered by the host renderer.
  on-event: func(event: string, payload: string) -> result<view, string>;
}
```

**Why view descriptors and not React component injection:** desktop renders React, server renders JSX, mobile renders React Native. A common descriptor format is the only thing that survives all three. Authors lose pixel-level control; the host enforces consistent styling. This is intentional — it's the same trade-off Slack made for blocks and VS Code made for tree views, and it scales.

### 6.3 Host APIs

Plugins import three host capabilities, declared in their manifest and gated at instantiation:

| Import                        | What it does                                                            | Sensitive?                                    |
| ----------------------------- | ----------------------------------------------------------------------- | --------------------------------------------- |
| `waveflow:host/http@1.0.0`    | Async HTTPS GET / POST with a per-plugin allow-list of origins.         | Yes — controls who the plugin talks to.       |
| `waveflow:host/log@1.0.0`     | `info`, `warn`, `error` log functions routed to the host tracing crate. | No — log only.                                |
| `waveflow:host/storage@1.0.0` | Key-value store scoped to the plugin. ~1 MB cap default.                | No — sandboxed to the plugin's own namespace. |

WASI 0.2 `wasi:filesystem` and `wasi:sockets` are **not** exposed in v1. A plugin that needs to read the user's filesystem (e.g., a local-files-elsewhere source) has to wait for v2.

**HTTP allow-list rationale:** the manifest declares `allowed-hosts = ["api.musicbrainz.org", "musicbrainz.org"]`; the host wraps the WASI HTTP outbound bindings and rejects anything outside the list with `permission-denied`. This is what stops a metadata enricher from quietly exfiltrating the user's listening history to a third domain.

### 6.4 Manifest schema

```toml
# ~/.config/waveflow/plugins/webradio/manifest.toml
name           = "Web Radio"
id             = "io.waveflow.webradio"                     # reverse-DNS, immutable
version        = "0.3.1"                                    # semver
authors        = ["jane.doe@example.com"]
homepage       = "https://github.com/jane/waveflow-webradio"
license        = "MIT"
description    = "Stream community-curated internet radio stations."

# Which WIT world this plugin implements. One of:
#   source / metadata / ui
plugin-type    = "source"

# Path to the WASM artifact, relative to the manifest.
artifact       = "plugin.wasm"

# Host APIs the plugin needs. The host rejects any export the plugin
# tries to use that isn't on this list.
[permissions]
allowed-hosts  = ["api.radio-browser.info", "*.radio-browser.info"]
storage-bytes  = 1_048_576    # 1 MiB — default cap if omitted

# Plugin-specific config exposed in Settings → Plugins → Web Radio.
# Keys here are passed back to the plugin via the `storage` host API
# under a reserved `__config__` namespace so the plugin doesn't need
# its own form-rendering code.
[config]
country = { type = "string", default = "FR", label = "Default country" }
```

The host fingerprints the WASM artifact (BLAKE3) and stores `(id, version, hash, last-loaded-at)` in `app_setting['plugin.<id>']`. A different hash for the same `(id, version)` triggers a `plugin replaced — review permissions` notification at next launch; the user must confirm before the plugin loads.

### 6.5 Lifecycle

```text
Boot:
  1. Scan ~/.config/waveflow/plugins/*/manifest.toml
  2. For each:
     a. Verify hash against the last-known store
     b. Instantiate wasmtime::Component
     c. Type-check exports against the declared `plugin-type`
     d. Run the `init` export with the manifest's `[config]` block
  3. Register live plugins with the matching trait registry in core

Run:
  - Source providers: invoked by the player when the user picks a non-library track
  - Metadata enrichers: invoked by the scanner / library refresh after Deezer + Last.fm
  - UI extensions: invoked by the React layer when the user navigates to the
    plugin's mount-point

Shutdown:
  - Drop all wasmtime Stores. WASI resources auto-close.
  - Persist `last-loaded-at` so a plugin that crashed during boot can be
    detected next launch.
```

**Fuel and per-call timeouts.** Every guest call is wrapped with a wasmtime fuel limit + a tokio timeout (configurable, default 10 s for source / metadata calls, 200 ms for UI `render`). A timeout cancels the call and surfaces a host-side error; the plugin instance survives so the next call works. Three consecutive timeouts disable the plugin for the rest of the session and surface a toast.

### 6.6 Desktop ↔ server parity

The plugin stack lives entirely inside `waveflow-core`. Both the desktop app and `waveflow-server` (RFC-001 Phase 1.b+) load plugins from the same directory layout on their respective hosts; the only difference is where that directory is:

| Host                  | Plugin directory                                                                                                                |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Desktop (Linux/macOS) | `$XDG_CONFIG_HOME/waveflow/plugins/`                                                                                            |
| Desktop (Windows)     | `%APPDATA%/waveflow/plugins/`                                                                                                   |
| Server                | `$WAVEFLOW_PLUGIN_DIR` (env, default `/var/lib/waveflow/plugins/`); per-user overrides under `<dir>/users/<user-id>/` for v1.5+ |

UI extensions render their view descriptors **on the client**: desktop turns them into React components, the web frontend (TanStack Start, RFC-001 §6.3) turns the same descriptors into JSX. The server hosts the plugin but never has to render — it just exposes a JSON endpoint that returns the descriptor.

### 6.7 Author DX

```bash
# Scaffold a new source-provider plugin
cargo install cargo-generate
cargo generate --git https://github.com/InstaZDLL/waveflow-plugin-template \
               --name my-radio --branch source-v1

cd my-radio
# Author writes Rust, generates the WIT bindings, builds to component
cargo build --release --target wasm32-wasip2
wasm-tools component new target/wasm32-wasip2/release/my_radio.wasm \
  -o my_radio.component.wasm

# Run inside the SDK's standalone test harness (no Tauri / no server)
waveflow-plugin dev ./my_radio.component.wasm manifest.toml
```

The `waveflow-plugin dev` CLI ships in the `waveflow-plugin-sdk` crate. It loads a plugin into the same `PluginHost` the production app uses, exposes a CLI prompt that calls the WIT exports interactively, and prints view-descriptor JSON for UI plugins. This is the only supported authoring loop — we deliberately do not provide a "load this directory and hot-reload on save" feature in v1 to keep the runtime surface tight.

A `cargo-component` wrapper / Makefile is provided in the template so authors don't need to remember the exact `wasm-tools component new` invocation.

### 6.8 First-party plugins (v1.5.0)

Exactly one plugin ships in v1.5.0 to validate the SDK end-to-end:

- **Web Radio** (issue [#171](https://github.com/InstaZDLL/WaveFlow/issues/171)) — source provider on the [Radio-Browser.info](https://www.radio-browser.info/) public API. Lists stations by country / genre / popularity, resolves the playable stream URL on demand. Ships as a separate `InstaZDLL/waveflow-plugin-webradio` repo.

Metadata enricher and UI extension examples (a MusicBrainz plugin and a "DJ sets" listing view) are scaffolded as **examples** under `crates/plugin-sdk/examples/` so authors have working references, but are not shipped as first-party.

## 7. Phase 3 delivery plan

This RFC lives inside Phase 3 of the project roadmap. Sub-phases mirror RFC-001's structure — each shippable independently, no flag-day cutover.

| Phase   | Scope                                                                                                                                                 | Repos touched     | Visible to user?                  |
| ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------- | --------------------------------- |
| **3.a** | `crates/plugin-sdk/` skeleton: WIT files, Rust binding crate, `waveflow-plugin dev` harness, `cargo generate` template. No host runtime yet.          | `waveflow`        | No (SDK is for authors).          |
| **3.b** | `waveflow-core::plugin` host: wasmtime runtime, manifest loader, sideload directory scan, lifecycle, host APIs (http + log + storage), fuel timeouts. | `waveflow`        | Hidden "Plugins" page in Settings |
| **3.c** | Source-provider trait wiring: plugin tracks show up in the library search results, play through the engine via on-demand `stream-url`.                | `waveflow`        | Web Radio works on desktop        |
| **3.d** | Metadata-enricher trait wiring: scanner + library refresh pipeline call enabled enrichers after Deezer / Last.fm; per-plugin fallback ranking.        | `waveflow`        | Optional MusicBrainz plugin       |
| **3.e** | UI-extension trait + descriptor renderer in `src/components/plugin/`. Settings → Plugins page (enable / disable, view permissions, plugin config).    | `waveflow`        | Plugins UI live                   |
| **3.f** | Mirror the runtime into `waveflow-server` (RFC-001). Same code, different plugin directory + per-user paths.                                          | `waveflow-server` | Plugins on web                    |

Estimated cadence: ~2 weeks per sub-phase = ~3 months for Phase 3. Phases 3.a → 3.c are the v1.5.0 cut; 3.d → 3.f land in 1.6.0+.

## 8. Open questions (deferred)

| Question                                                                                  | Defer to | Why deferred                                                                                                                            |
| ----------------------------------------------------------------------------------------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Curated registry: discovery surface, signing model, abuse policy.                         | v1.7+    | Need ≥ 3 third-party plugins in the wild before designing a registry — design without real data leads to over-engineering.              |
| Audio DSP plugin world (`waveflow:dsp/v1`).                                               | TBD      | The real-time audio thread doesn't allow general WASM. Solving this needs research into compile-time-validated DSP graphs (Faust-like). |
| Cross-plugin RPC.                                                                         | TBD      | No use case yet. Don't add features looking for users.                                                                                  |
| Hot reload of running plugins during authoring.                                           | 3.b      | A stretch goal of the `waveflow-plugin dev` harness, not the production host.                                                           |
| Plugin auto-update (sideload still, but pull manifest version diffs from a user-set URL). | v1.7+    | Lower priority than registry; users can update by replacing files.                                                                      |
| What happens when two source plugins claim the same track id.                             | 3.c      | Need a real conflict to design — most likely first-loaded wins, surface a warning.                                                      |

## 9. Risks

| Risk                                                                                     | Likelihood | Impact | Mitigation                                                                                                                                                                           |
| ---------------------------------------------------------------------------------------- | ---------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Component Model toolchain breaking changes during 3.a → 3.c                              | Medium     | Medium | Pin `wit-bindgen` and `wasmtime` to exact versions; treat upgrades as scheduled work. Keep the WIT files versioned (`waveflow:source@1.0.0`) so authors aren't broken by host bumps. |
| Binary size blowup from wasmtime adds 12+ MB to the bundle                               | High       | Low    | Already accepted. Document in the Linux package descriptions. Cut elsewhere (e.g., feature-gate Deezer in `--no-default-features` server builds) if budget tightens.                 |
| Web Radio plugin can't keep up with Radio-Browser API instability                        | Medium     | Medium | Web Radio plugin ships as a separate repo on its own release cadence; SDK can roll forward independently. Plugin caches station lists to weather upstream outages.                   |
| UI descriptor DSL is too restrictive — authors want bespoke React                        | Medium     | Medium | The DSL is intentionally limited in v1. If real plugins hit the ceiling, v2 adds a `block-html` view variant that renders sanitized HTML in a Shadow DOM iframe — opt-in, scoped.    |
| A malicious plugin abuses `waveflow:host/http` allow-list to chain through an open proxy | Low        | High   | The host's HTTP impl rejects non-CONNECT proxies and validates the resolved IP isn't private (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`, `127.0.0.0/8`) before dialing.        |
| Plugin crash cascades into the host process                                              | Low        | High   | Each plugin runs in its own `wasmtime::Store` with fuel limits + panic = trap. A trap surfaces as `Result::Err` to core; the plugin instance is dropped, the host continues.         |

## 10. Alternatives considered

| Alternative                                           | Why rejected                                                                                                                                                      |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Dynamic libraries (`.so` / `.dll` / `.dylib`)         | Native speed, but no sandbox. A bad plugin segfaults the audio engine. Not portable across desktop / server hosts.                                                |
| JavaScript sandbox (Deno-style isolates)              | Doubles the runtime footprint (V8 alongside wasmtime would be ~30 MB extra). WASM Component Model already accepts JS via `componentize-js` without that overhead. |
| Lua scripting                                         | Same JS argument but smaller. Loses cross-language compat (no Rust / Go authoring). Custom ABI work for every host call.                                          |
| Tauri-style plugin (Rust crate compiled into the app) | Forces authors to rebuild WaveFlow. Not user-installable. Defeats the whole purpose.                                                                              |
| Webhook-style plugins (HTTP server somewhere else)    | Latency, deployment complexity, makes offline-only WaveFlow no longer offline-only. Acceptable for power users but not as the default model.                      |
| Inline React component injection for UI extensions    | Couples plugin authors to the desktop's React version; impossible on the server without an SSR rendering pass per request. View descriptors decouple cleanly.     |

## 11. Glossary

- **Component Model**: WASI-aligned ABI on top of WebAssembly that adds typed, multi-language interfaces (interface types, resources, async). Defined by the Bytecode Alliance.
- **fuel (wasmtime)**: a per-call execution budget; the host charges WASM instructions against it and traps the call when the budget runs out.
- **`jco` / `componentize-js`**: official toolchain for compiling JavaScript / TypeScript to WASM components.
- **sideload**: installing a plugin by manually placing files in the user's plugin directory, bypassing any registry.
- **trap**: a WASM-level exception (out-of-bounds, fuel exhaustion, explicit `unreachable`); always recoverable on the host side.
- **WASI 0.2**: the snapshot of WebAssembly System Interface APIs released alongside Component Model stabilization. Provides `wasi:http`, `wasi:filesystem`, `wasi:sockets`, etc.
- **WIT (WebAssembly Interface Type)**: the IDL Component Model uses to describe imports and exports. Sibling format to Protobuf / Thrift / Cap'n Proto.

---

**Next step after acceptance**: open the Phase 3 milestone and tracking issues, then start 3.a (`crates/plugin-sdk/` skeleton) in parallel with whatever RFC-001 sub-phase is in flight. The SDK scaffolding has zero dependency on the server, so the two tracks proceed independently.
