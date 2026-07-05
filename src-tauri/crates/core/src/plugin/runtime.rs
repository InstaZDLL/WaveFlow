//! Wasmtime runtime skeleton (Phase 2a).
//!
//! This module owns the process-wide [`Engine`] + the per-plugin
//! sandbox knobs (fuel, memory cap, epoch interrupt) and resolves a
//! plugin id on disk into a compiled [`Component`] paired with its
//! parsed manifest. It intentionally does NOT yet bind any host
//! imports — Phase 2b adds the `waveflow:host/{http,log,storage}`
//! impls + the `bindgen!` wiring + the per-world [`Linker`]. The
//! intent is a reviewable foundation: a host can `load_plugin` and
//! confirm the wasm parses, before any guest code is actually
//! instantiated.
//!
//! Sandbox defaults — see [`RuntimeConfig`] for the values. They're
//! aggressive (64 MB / 100M fuel / 30 s epoch deadline) compared to
//! a generic wasm host because a music-player plugin is expected to
//! do small bursts of I/O around an HTTP fetch + a parse, not a
//! long-running compute task. The host can raise them per plugin
//! later; Phase 2a only exposes the defaults.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, OptLevel, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::plugin::assets::AssetResolver;
use crate::plugin::host_impl::{HostError, HostPermissions, StateStore, STATE_QUOTA_BYTES};
use crate::plugin::manifest::{Manifest, ManifestError};
use crate::plugin::{InvalidPluginId, PluginPaths};

/// Process-wide "offline mode" probe. Returns `true` when the host
/// has flipped offline mode (`app_setting['network.offline_mode']`
/// on the desktop side, equivalent gate on the server side). Every
/// plugin HTTP call short-circuits to an empty 503 response while
/// the probe returns `true`, matching the convention CLAUDE.md
/// spells for every other outbound HTTP path (Deezer, Last.fm,
/// LRCLIB, similar).
///
/// `waveflow-core` doesn't own user-facing settings — those live in
/// the host crates (`waveflow`, `waveflow-server`). The host
/// injects its real probe via [`PluginRuntime::new_with_offline_probe`];
/// the default [`PluginRuntime::new`] uses an always-online stub so
/// `core` tests don't need to thread a probe through every call.
pub type OfflineProbe = Arc<dyn Fn() -> bool + Send + Sync>;

fn always_online() -> OfflineProbe {
    Arc::new(|| false)
}

/// Hard cap on the compiled `plugin.wasm` size we'll read off disk.
/// Defence-in-depth against a hostile sideload: a multi-GB file
/// would OOM the host long before wasmtime ever got a chance to
/// reject it. 50 MB is comfortable headroom for our largest planned
/// plugin (Web Radio with a ~10 MB SQLite + icon cache + bindings,
/// ballparked at 15-20 MB compiled); anything bigger is suspect and
/// gets refused at the syscall boundary.
const MAX_WASM_SIZE: u64 = 50 * 1024 * 1024;

/// Sandbox limits applied to every plugin Store this runtime spins
/// up. Cloning is cheap (three integers); the runtime takes one of
/// these by value at construction and hands it to each Store.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Max fuel units a guest call may consume before trapping.
    /// Wasmtime decrements fuel on every wasm instruction; a fuel
    /// trap aborts the call cleanly without affecting other plugins
    /// or the host. 100M ≈ a few seconds of pure compute on typical
    /// hardware — comfortable headroom for an HTTP-bound plugin.
    pub fuel_per_call: u64,

    /// Max linear-memory bytes the guest may allocate. Enforced via
    /// [`ResourceLimiter`] — an over-cap `memory.grow` returns 0 to
    /// the guest (the standard wasm out-of-memory signal). 64 MB
    /// covers a Web Radio plugin embedding a SQLite cache + an icon
    /// resize buffer with margin.
    pub max_memory_bytes: usize,

    /// Number of epoch ticks a guest call may pass through before
    /// being interrupted. The host runs a 10 ms ticker that calls
    /// [`Engine::increment_epoch`], so the default 3_000 ticks ≈ 30 s
    /// wall-clock — long enough to swallow a slow network round-trip
    /// without strangling a plugin that's intentionally polling.
    pub epoch_ticks_per_call: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            fuel_per_call: 100_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            epoch_ticks_per_call: 3_000,
        }
    }
}

/// Errors that can surface from the plugin runtime. Each variant is
/// either a wasmtime-side failure (compile / instantiate / trap) or
/// a path / manifest issue we caught before touching wasm.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("invalid plugin id: {0}")]
    InvalidId(#[from] InvalidPluginId),
    #[error(
        "manifest plugin.id {found:?} does not match install dir {expected:?} — refusing to load"
    )]
    PluginIdMismatch { expected: String, found: String },
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("wasmtime: {0}")]
    Wasmtime(#[from] wasmtime::Error),
    #[error("plugin.wasm too large: at least {size} bytes (max {max})")]
    WasmTooLarge { size: u64, max: u64 },
    #[error("plugin.wasm is not a regular file")]
    WasmNotRegularFile,
    #[error("host: {0}")]
    Host(#[from] HostError),
    #[error("http client init: {0}")]
    HttpClient(#[from] reqwest::Error),
}

/// Process-wide plugin runtime. Owns the [`Engine`] (`Send + Sync`
/// across threads) plus the shared sandbox config. One instance per
/// app — cheap to clone via [`Arc`] when handing into a tokio task.
#[derive(Clone)]
pub struct PluginRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    engine: Engine,
    config: RuntimeConfig,
    /// Shared blocking HTTP client. Built ONCE here with redirect
    /// following disabled so a plugin's `permissions.http` allowlist
    /// can't be bypassed by an allowlisted host returning a 302 to
    /// an arbitrary URL — reqwest's default policy follows up to 10
    /// hops, the allowlist only gates the initial `req.url`, so
    /// without this we'd grant transitive access to anywhere the
    /// allowlisted server cared to redirect to. The plugin sees the
    /// 3xx + `Location` header in the response and re-issues
    /// through the allowlist if it wants to follow.
    ///
    /// Cloning the client is cheap — reqwest wraps its connection
    /// pool in `Arc` internally, so per-plugin Stores share one
    /// pool and reuse warm TLS sessions.
    http_client: reqwest::blocking::Client,
    /// See [`OfflineProbe`]. Cloned into every `HostCtx`
    /// `new_store_for_plugin` builds, so a probe flip takes effect
    /// on the next plugin instantiation without rebuilding the
    /// runtime.
    offline_probe: OfflineProbe,
}

impl PluginRuntime {
    /// Build a runtime with the given sandbox config and the
    /// always-online offline stub. Use [`Self::new_with_offline_probe`]
    /// from the host crate to plumb the real
    /// `app_setting['network.offline_mode']` flag.
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        Self::new_with_offline_probe(config, always_online())
    }

    /// Same as [`Self::new`] but accepts a process-wide offline
    /// probe. Phase 3's host wiring passes its real
    /// `offline::is_offline` closure here.
    pub fn new_with_offline_probe(
        config: RuntimeConfig,
        offline_probe: OfflineProbe,
    ) -> Result<Self, RuntimeError> {
        let mut wt = Config::new();
        // Component Model is the only execution mode plugins use —
        // raw wasm modules aren't a supported plugin shape.
        wt.wasm_component_model(true);
        // Fuel + epoch are the two independent sandboxes: fuel
        // catches infinite loops at the instruction level, epoch
        // catches blocked host calls (an import that never returns)
        // at wall-clock granularity. Both are required because
        // neither alone catches the other's failure mode.
        wt.consume_fuel(true);
        wt.epoch_interruption(true);
        // Threads stay off because the crate doesn't enable
        // wasmtime's `threads` Cargo feature — there's no `Config`
        // toggle to disable what isn't compiled in. If a future
        // change opts that feature in, add `wt.wasm_threads(false)`
        // back here so a plugin can never spin up wasm threads
        // outside the host's resource accounting.
        // Plugins are loaded once and called many times; spend the
        // compile budget on faster steady-state code instead of
        // shorter compile time.
        wt.cranelift_opt_level(OptLevel::Speed);

        let engine = Engine::new(&wt)?;
        // Timeouts are the second-line bound on a misbehaving host.
        // The wasmtime epoch deadline (~30 s by default) would
        // eventually trap a request that hangs, but the trap takes
        // down the whole guest call rather than letting the plugin
        // surface a clean error. A 15 s request timeout + 5 s
        // connect timeout returns a recoverable `reqwest::Error`
        // the plugin sees as `Err(...)` while the epoch is left as
        // the last-resort sandbox catch.
        let http_client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                engine,
                config,
                http_client,
                offline_probe,
            }),
        })
    }

    /// Shared [`Engine`] handle — pass to `bindgen!`-generated
    /// `add_to_linker` helpers in Phase 2b.
    pub fn engine(&self) -> &Engine {
        &self.inner.engine
    }

    /// Sandbox config used for every Store spun up by this runtime.
    pub fn config(&self) -> &RuntimeConfig {
        &self.inner.config
    }

    /// Bump the engine's epoch counter. The host runs a dedicated
    /// timer thread (Phase 2b wires the timer alongside the host
    /// imports) that calls this every 10 ms; we expose it here so
    /// tests can drive the epoch deterministically.
    pub fn tick_epoch(&self) {
        self.inner.engine.increment_epoch();
    }

    /// Read + parse a plugin's manifest, compile its `plugin.wasm`,
    /// and return both bundled together. The function does NOT
    /// instantiate the component — Phase 2b is where a `Linker` +
    /// `Store` come together and the guest's exported functions
    /// become callable.
    ///
    /// The manifest's declared `world` is validated here against the
    /// SDK's known catalog ([`Manifest::parse`] enforces this), but
    /// we don't yet cross-check that the compiled component
    /// actually exports the declared interfaces — that check needs
    /// the bindgen-generated types and lands in Phase 2b.
    pub fn load_plugin(
        &self,
        paths: &PluginPaths,
        plugin_id: &str,
    ) -> Result<LoadedPlugin, RuntimeError> {
        let manifest_path = paths.manifest_path(plugin_id)?;
        let manifest = Manifest::load_from_path(&manifest_path)?;
        // Pin the manifest's declared id to the install-dir id.
        // Without this check the install path (`plugins/<dir>/`)
        // and the runtime id (`manifest.plugin.id`) can drift — a
        // plugin installed under `plugins/foo/` but declaring
        // `id = "bar"` would later read assets from `plugins/bar/`
        // and write state into `plugin-data/bar/` once
        // `new_store_for_plugin` keys everything off the manifest
        // id. Refuse at load time so the host never instantiates a
        // mismatched layout.
        if manifest.plugin.id != plugin_id {
            return Err(RuntimeError::PluginIdMismatch {
                expected: plugin_id.to_string(),
                found: manifest.plugin.id.clone(),
            });
        }
        let wasm_path = paths.wasm_path(plugin_id)?;
        // Open once, stat the same handle, read from the same handle.
        // Separating `fs::metadata` from `fs::read` is a TOCTOU window
        // (a sideloaded plugin could swap the file between the two
        // syscalls) AND `metadata.len()` reports 0 for FIFOs / sockets
        // / character devices — those would slip past a pre-stat size
        // check and stream unbounded bytes into our `Vec`. Open the
        // handle, confirm it's a regular file, then bound the read at
        // `MAX_WASM_SIZE + 1` so anything over the cap is detected
        // without ever holding more than `MAX_WASM_SIZE + 1` bytes
        // in memory.
        let file = std::fs::File::open(&wasm_path)?;
        let meta = file.metadata()?;
        if !meta.file_type().is_file() {
            return Err(RuntimeError::WasmNotRegularFile);
        }
        let mut bytes = Vec::with_capacity(meta.len().min(MAX_WASM_SIZE) as usize);
        let read = file.take(MAX_WASM_SIZE + 1).read_to_end(&mut bytes)? as u64;
        if read > MAX_WASM_SIZE {
            return Err(RuntimeError::WasmTooLarge {
                size: read,
                max: MAX_WASM_SIZE,
            });
        }
        let component = Component::from_binary(&self.inner.engine, &bytes)?;
        Ok(LoadedPlugin {
            manifest,
            component,
            wasm_path,
        })
    }

    /// Construct a fresh [`Store`] for one plugin invocation,
    /// pre-loaded with the sandbox limits + the host context
    /// derived from `loaded` and `paths`. The HostCtx pulls in the
    /// manifest's permission snapshot, the per-plugin asset
    /// resolver (when assets are declared), the scratch-store
    /// handle, and a shared reqwest blocking client.
    ///
    /// The plugin id used to resolve on-disk paths + scope log
    /// events + key the scratch store comes from
    /// `loaded.manifest.plugin.id` — one canonical source. An
    /// earlier signature took an extra `plugin_id: &str` parameter
    /// alongside the LoadedPlugin, which let the install-dir id and
    /// the manifest id drift apart (assets would resolve against
    /// one tree while permissions + log scope referenced another).
    /// The manifest validator already restricts the id to
    /// `[a-z0-9-]+`, and `PluginPaths` re-checks the shape via
    /// `sanitise_id`, so leaning on the manifest id is safe.
    pub fn new_store_for_plugin(
        &self,
        loaded: &LoadedPlugin,
        paths: &PluginPaths,
    ) -> Result<Store<HostCtx>, RuntimeError> {
        let plugin_id = loaded.manifest.plugin.id.as_str();
        let plugin_dir = paths.plugin_dir(plugin_id)?;
        let state_dir = paths.state_dir(plugin_id)?;
        let assets = if loaded.manifest.assets.is_empty() {
            None
        } else {
            Some(AssetResolver::new(&plugin_dir, &loaded.manifest))
        };
        let permissions = HostPermissions::from_manifest(&loaded.manifest)?;
        let state = StateStore::new(state_dir, STATE_QUOTA_BYTES);
        // Clone the runtime's redirect-disabled client (cheap — the
        // connection pool sits behind an internal Arc, so per-Store
        // clones share one warm TLS / DNS cache across plugins).
        let http_client = self.inner.http_client.clone();
        let offline_probe = Arc::clone(&self.inner.offline_probe);
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.inner.config.max_memory_bytes)
            .build();

        let ctx = HostCtx {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            limits,
            plugin_id: plugin_id.to_string(),
            permissions,
            assets,
            state,
            http_client,
            offline_probe,
        };
        let mut store = Store::new(&self.inner.engine, ctx);

        store.set_fuel(self.inner.config.fuel_per_call)?;
        store.set_epoch_deadline(self.inner.config.epoch_ticks_per_call);
        store.limiter(|ctx| &mut ctx.limits);

        Ok(store)
    }

    /// Build a [`Linker`] pre-populated with every
    /// `waveflow:host/*` import the SDK ships PLUS the minimal
    /// WASI P2 surface every `cargo component`-built plugin
    /// implicitly imports for panic / stdio / clock glue. One
    /// linker covers every store this runtime spins up — wasmtime's
    /// `Linker<HostCtx>` is `Send + Sync` and `Component::instantiate`
    /// only reads it, so the caller can keep a single `Arc<Linker>`
    /// around and clone-share it across instantiations.
    pub fn build_linker(&self) -> Result<Linker<HostCtx>, RuntimeError> {
        let mut linker = Linker::<HostCtx>::new(&self.inner.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
        crate::plugin::host_impl::add_to_linker(&mut linker)?;
        Ok(linker)
    }
}

// ----- source-v1 invocation helpers ---------------------------------------
//
// Loads + instantiates + calls the `waveflow:source/provider`
// exports. Returns owned DTOs so the caller (Tauri commands, future
// waveflow-server handlers) doesn't have to depend on wasmtime
// itself — every plumbing crate the host needs lives behind these
// three free functions.

/// Owned mirror of `waveflow:source/provider/entry` — what the
/// plugin returns from `list-entries`.
#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub label: String,
    pub query: String,
    pub icon_url: Option<String>,
}

/// Owned mirror of `waveflow:source/provider/track` — what the
/// plugin returns from `resolve`.
#[derive(Debug, Clone)]
pub struct SourceTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: u32,
    pub artwork_url: Option<String>,
    pub icy_url: Option<String>,
}

/// Errors specific to the source-invocation surface. Distinct from
/// [`RuntimeError`] so callers can tell a plugin-side `Err` from a
/// host-side trap / load failure.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("runtime: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("instantiate: {0}")]
    Instantiate(String),
    #[error("trap: {0}")]
    Trap(String),
    #[error("plugin: {0}")]
    Plugin(String),
}

fn instantiate_source(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
) -> Result<(Store<HostCtx>, crate::plugin::bindings::source::Plugin), SourceError> {
    let loaded = runtime.load_plugin(paths, plugin_id)?;
    let linker = runtime.build_linker()?;
    let mut store = runtime.new_store_for_plugin(&loaded, paths)?;
    let plugin = crate::plugin::bindings::source::Plugin::instantiate(
        &mut store,
        &loaded.component,
        &linker,
    )
    .map_err(|e| SourceError::Instantiate(e.to_string()))?;
    Ok((store, plugin))
}

/// Call the guest's `list-entries`.
pub fn source_list_entries(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
) -> Result<Vec<SourceEntry>, SourceError> {
    let (mut store, plugin) = instantiate_source(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_source_provider()
        .call_list_entries(&mut store)
        .map_err(|e| SourceError::Trap(e.to_string()))?;
    match result {
        Ok(entries) => Ok(entries
            .into_iter()
            .map(|e| SourceEntry {
                label: e.label,
                query: e.query,
                icon_url: e.icon_url,
            })
            .collect()),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

/// Call the guest's `resolve(query)`.
pub fn source_resolve(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    query: &str,
) -> Result<Vec<SourceTrack>, SourceError> {
    let (mut store, plugin) = instantiate_source(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_source_provider()
        .call_resolve(&mut store, query)
        .map_err(|e| SourceError::Trap(e.to_string()))?;
    match result {
        Ok(tracks) => Ok(tracks
            .into_iter()
            .map(|t| SourceTrack {
                id: t.id,
                title: t.title,
                artist: t.artist,
                album: t.album,
                duration_ms: t.duration_ms,
                artwork_url: t.artwork_url,
                icy_url: t.icy_url,
            })
            .collect()),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

/// Call the guest's `stream-url(track-id)`.
pub fn source_stream_url(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    track_id: &str,
) -> Result<String, SourceError> {
    let (mut store, plugin) = instantiate_source(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_source_provider()
        .call_stream_url(&mut store, track_id)
        .map_err(|e| SourceError::Trap(e.to_string()))?;
    result.map_err(SourceError::Plugin)
}

// ----- metadata-v1 invocation helpers -------------------------------------
//
// Instantiates + calls the `waveflow:metadata/enricher` exports. Phase 3
// wires only `album-info` (the motion-artwork path needs it); `artist-info`
// + `lyrics` get helpers when a plugin consumes them. Reuses [`SourceError`]
// — its Instantiate / Trap / Plugin variants are world-agnostic.

/// Owned mirror of `waveflow:metadata/enricher/album-info` (1.1.0).
#[derive(Debug, Clone, Default)]
pub struct AlbumInfo {
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub track_count: Option<u32>,
    /// Looping video URL for the square animated cover (HLS/mp4).
    pub motion_cover_url: Option<String>,
    /// Taller lock-screen variant; `None` falls back to the square.
    pub motion_cover_tall_url: Option<String>,
}

fn instantiate_metadata(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
) -> Result<(Store<HostCtx>, crate::plugin::bindings::metadata::Plugin), SourceError> {
    let loaded = runtime.load_plugin(paths, plugin_id)?;
    let linker = runtime.build_linker()?;
    let mut store = runtime.new_store_for_plugin(&loaded, paths)?;
    let plugin = crate::plugin::bindings::metadata::Plugin::instantiate(
        &mut store,
        &loaded.component,
        &linker,
    )
    .map_err(|e| SourceError::Instantiate(e.to_string()))?;
    Ok((store, plugin))
}

/// Call the guest's `album-info(artist, title)`.
pub fn metadata_album_info(
    runtime: &PluginRuntime,
    paths: &PluginPaths,
    plugin_id: &str,
    artist: &str,
    title: &str,
) -> Result<AlbumInfo, SourceError> {
    let (mut store, plugin) = instantiate_metadata(runtime, paths, plugin_id)?;
    let result = plugin
        .waveflow_metadata_enricher()
        .call_album_info(&mut store, artist, title)
        .map_err(|e| SourceError::Trap(e.to_string()))?;
    match result {
        Ok(info) => Ok(AlbumInfo {
            description: info.description,
            cover_url: info.cover_url,
            track_count: info.track_count,
            motion_cover_url: info.motion_cover_url,
            motion_cover_tall_url: info.motion_cover_tall_url,
        }),
        Err(msg) => Err(SourceError::Plugin(msg)),
    }
}

// ----- wasmtime_wasi WasiView wiring --------------------------------------

impl WasiView for HostCtx {
    /// Returns the combined ctx + resource-table view wasmtime_wasi
    /// 45 reads on every WASI import call. We share `self.table`
    /// with the bindgen-generated `waveflow:host/*` resources — both
    /// sides allocate into the same handle space, which is the
    /// expected pattern when a host implements multiple WIT
    /// interfaces against one Store.
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// One plugin loaded from disk: parsed manifest + compiled
/// component + the path the wasm came from (kept for diagnostics).
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub component: Component,
    pub wasm_path: PathBuf,
}

/// Per-Store host context. Owns everything the `waveflow:host/*`
/// import impls read from at call time: the permission snapshot
/// taken at instantiate, the asset resolver (when assets are
/// declared), the scratch-store handle, a shared HTTP client, and
/// the plugin id used for log scoping.
///
/// Fields that pin runtime invariants — `plugin_id`, `permissions`,
/// `assets`, `state`, `http_client`, `offline_probe` — are
/// `pub(crate)` so external crates (the app, the server) can't
/// rewrite them after instantiation and slip past the manifest's
/// permission gates, asset sandbox, scratch-store isolation, or
/// the runtime's redirect / timeout / offline policy on the HTTP
/// client. `table` + `limits` stay `pub` because wasmtime itself
/// reaches into them and there's no equivalent invariant to
/// protect on those.
pub struct HostCtx {
    /// Wasmtime resource table — required by `bindgen!` AND by
    /// `wasmtime_wasi::p2::IoView`. Shared by both: the same handle
    /// space backs guest-allocated resources from our `waveflow:host/*`
    /// imports and the WASI minimal-context streams.
    pub table: ResourceTable,
    /// Minimal WASI P2 context. The host adds `wasmtime_wasi` to
    /// every plugin's linker because `cargo component`'s default
    /// adapter implicitly imports `wasi:cli/environment`,
    /// `wasi:io/streams`, `wasi:clocks/wall-clock`, etc. for panic
    /// handling + stdio + clock APIs that wit-bindgen-rt's
    /// glue code calls. The context here is intentionally bare —
    /// no filesystem preopens, no env inheritance, no args. The
    /// `wasi:sockets/*` surface stays callable (wasmtime-wasi
    /// doesn't strip it), but every destination is denied by
    /// default because no `allow_*` rule is registered; together
    /// with the `waveflow:host/http` allowlist gating the only
    /// HTTP path we actually expose, the result is "no host
    /// resources reachable unless the manifest asked for them".
    pub(crate) wasi: WasiCtx,
    /// Memory cap enforcement. The [`Store::limiter`] closure points
    /// at this field so a guest `memory.grow` over the cap returns
    /// 0 instead of asking the OS for more pages.
    pub limits: StoreLimits,

    /// Manifest id for tracing scope. Every `waveflow:host/log.emit`
    /// call routes the message into the host's tracing subsystem
    /// with `plugin = <id>` set automatically. `pub(crate)` so
    /// outside callers can't relabel a running plugin.
    pub(crate) plugin_id: String,
    /// Compiled allowlist + storage flags pinned at instantiate
    /// time. See [`HostPermissions::from_manifest`]. `pub(crate)`
    /// so the manifest's gates can't be widened post-hoc — a host
    /// that wants different permissions builds a new store.
    pub(crate) permissions: HostPermissions,
    /// Sidecar asset reader. `None` when the manifest declared no
    /// `[[assets]]` table; the storage host impl returns a clear
    /// error in that case instead of pretending the resolver
    /// exists but is empty. `pub(crate)` so the asset sandbox
    /// can't be swapped from outside the runtime.
    pub(crate) assets: Option<AssetResolver>,
    /// Per-plugin scratch key/value store with the global 10 MB
    /// quota baked in. `pub(crate)` so an external caller can't
    /// swap the backend mid-flight to point at another plugin's
    /// scratch dir (or anywhere else on disk).
    pub(crate) state: StateStore,
    /// Blocking reqwest client. Sync because the WIT `http.send`
    /// signature is sync and bridging through tokio::block_on
    /// inside a wasmtime callback would deadlock on the same
    /// worker thread the guest is running on. `pub(crate)` so an
    /// external caller can't replace it with a client that follows
    /// redirects, removes timeouts, or otherwise undoes the
    /// runtime's HTTP policy.
    pub(crate) http_client: reqwest::blocking::Client,
    /// Cloned snapshot of the runtime's [`OfflineProbe`]. The HTTP
    /// host impl reads this on every `http.send` and short-circuits
    /// to an empty 503 response when it returns `true`, matching
    /// the convention every other outbound HTTP path in this
    /// workspace follows. `pub(crate)` so a caller can't pin the
    /// probe to a constant and bypass the host's offline switch.
    pub(crate) offline_probe: OfflineProbe,
}

impl HostCtx {
    /// Stub builder for unit tests that don't go through
    /// `PluginRuntime::new_store_for_plugin`. Empty permissions,
    /// no assets, an isolated scratch dir under the OS temp tree,
    /// fresh reqwest client.
    #[cfg(test)]
    pub fn stub(max_memory_bytes: usize, state_dir: PathBuf) -> Self {
        let limits = StoreLimitsBuilder::new()
            .memory_size(max_memory_bytes)
            .build();
        Self {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            limits,
            plugin_id: "test-plugin".into(),
            permissions: HostPermissions::empty(),
            assets: None,
            state: StateStore::new(state_dir, STATE_QUOTA_BYTES),
            // Match the production redirect policy + timeouts so
            // tests don't accidentally pass with permissive defaults
            // the production runtime explicitly disables.
            http_client: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("redirect-disabled client builds"),
            offline_probe: always_online(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_defaults_are_sensible() {
        let cfg = RuntimeConfig::default();
        assert!(cfg.fuel_per_call > 0);
        assert!(cfg.max_memory_bytes >= 16 * 1024 * 1024);
        assert!(cfg.epoch_ticks_per_call > 0);
    }

    #[test]
    fn runtime_builds_with_default_config() {
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        assert_eq!(runtime.config().max_memory_bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn store_carries_sandbox_limits() {
        // Builds a Store manually (bypassing `new_store_for_plugin`,
        // which needs an on-disk plugin) so the sandbox handoff —
        // fuel + epoch + memory limiter — is testable in isolation.
        let tmp = tempfile::tempdir().expect("tempdir");
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        let ctx = HostCtx::stub(runtime.config().max_memory_bytes, tmp.path().to_path_buf());
        let mut store = Store::new(runtime.engine(), ctx);
        store
            .set_fuel(runtime.config().fuel_per_call)
            .expect("fuel set");
        store.set_epoch_deadline(runtime.config().epoch_ticks_per_call);
        store.limiter(|ctx| &mut ctx.limits);

        let fuel = store.get_fuel().expect("store has fuel enabled");
        assert_eq!(fuel, runtime.config().fuel_per_call);
    }

    #[test]
    fn epoch_tick_increments_engine() {
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        // No public getter for the epoch counter — just confirm
        // the call doesn't panic. Phase 2b's integration test will
        // verify the deadline actually traps a guest.
        runtime.tick_epoch();
        runtime.tick_epoch();
    }

    #[test]
    fn build_linker_succeeds() {
        // Regression: if any host import wires up wrong (signature
        // drift between WIT + impl, missing trait, double-add) the
        // generated `add_to_linker` returns an error or panics.
        // The handshake is silent — we just need it to succeed.
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        let linker = runtime.build_linker();
        assert!(linker.is_ok(), "linker build failed: {:?}", linker.err());
    }

    #[test]
    fn load_plugin_rejects_invalid_id() {
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        let paths = PluginPaths::from_app_data(std::path::Path::new("/tmp/waveflow"));
        // `LoadedPlugin` doesn't impl `Debug` (wasmtime's `Component`
        // doesn't either), so we can't `.unwrap_err()` or print
        // the Ok branch — collapse the result to a tagged enum the
        // assertion can compare against.
        match runtime.load_plugin(&paths, "../escape") {
            Ok(_) => panic!("expected error, got Ok"),
            Err(RuntimeError::InvalidId(_)) => {}
            Err(err) => panic!("expected InvalidId, got {err:?}"),
        }
    }

    #[test]
    fn load_plugin_refuses_oversized_wasm() {
        // Lay down a valid manifest + a sparse "plugin.wasm" whose
        // file length exceeds `MAX_WASM_SIZE`. We use `set_len` on
        // an empty file so the disk footprint stays trivial — the
        // guard reads `fs::metadata().len()` which reflects the
        // logical (sparse) size, not allocated blocks.
        let tmp = tempfile::tempdir().expect("tempdir");
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        let paths = PluginPaths::from_app_data(tmp.path());
        let plugin_dir = paths.plugin_dir("oversize").expect("dir");
        std::fs::create_dir_all(&plugin_dir).expect("mkdir");

        let manifest = r#"
schema_version = 1

[plugin]
id = "oversize"
name = "x"
version = "1"
author = "x"
world = "waveflow:source/v1"
"#;
        std::fs::write(plugin_dir.join("manifest.toml"), manifest).expect("write manifest");

        let wasm = std::fs::File::create(plugin_dir.join("plugin.wasm")).expect("create wasm");
        wasm.set_len(MAX_WASM_SIZE + 1).expect("set_len");
        drop(wasm);

        match runtime.load_plugin(&paths, "oversize") {
            Ok(_) => panic!("expected error, got Ok"),
            Err(RuntimeError::WasmTooLarge { size, max }) => {
                assert_eq!(size, MAX_WASM_SIZE + 1);
                assert_eq!(max, MAX_WASM_SIZE);
            }
            Err(err) => panic!("expected WasmTooLarge, got {err:?}"),
        }
    }

    #[test]
    fn load_plugin_rejects_id_mismatch_between_dir_and_manifest() {
        // Lay down a plugin under `plugins/foo/` whose manifest
        // declares `id = "bar"`. Without the load-time pin, the
        // runtime would happily go on to read assets from
        // `plugins/bar/assets/` and write state to
        // `plugin-data/bar/` once `new_store_for_plugin` keys off
        // the manifest id — the install dir effectively forgers
        // a different plugin's resources.
        let tmp = tempfile::tempdir().expect("tempdir");
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        let paths = PluginPaths::from_app_data(tmp.path());

        let install_dir = paths.plugin_dir("foo").expect("dir");
        std::fs::create_dir_all(&install_dir).expect("mkdir");
        let manifest = r#"
schema_version = 1

[plugin]
id = "bar"
name = "x"
version = "1"
author = "x"
world = "waveflow:source/v1"
"#;
        std::fs::write(install_dir.join("manifest.toml"), manifest).expect("write manifest");
        // Empty wasm — the load-time check fires before we read it.
        std::fs::write(install_dir.join("plugin.wasm"), b"").expect("touch wasm");

        match runtime.load_plugin(&paths, "foo") {
            Ok(_) => panic!("expected error, got Ok"),
            Err(RuntimeError::PluginIdMismatch { expected, found }) => {
                assert_eq!(expected, "foo");
                assert_eq!(found, "bar");
            }
            Err(err) => panic!("expected PluginIdMismatch, got {err:?}"),
        }
    }

    #[test]
    fn load_plugin_reports_missing_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        let paths = PluginPaths::from_app_data(tmp.path());
        // No files on disk — `manifest_path` points nowhere valid.
        match runtime.load_plugin(&paths, "ghost") {
            Ok(_) => panic!("expected error, got Ok"),
            Err(RuntimeError::Io(_)) | Err(RuntimeError::Manifest(_)) => {}
            Err(err) => panic!("expected Io / Manifest, got {err:?}"),
        }
    }
}
