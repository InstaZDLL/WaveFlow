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

use std::path::PathBuf;
use std::sync::Arc;

use wasmtime::component::{Component, ResourceTable};
use wasmtime::{Config, Engine, OptLevel, Store, StoreLimits, StoreLimitsBuilder};

use crate::plugin::manifest::{Manifest, ManifestError};
use crate::plugin::{InvalidPluginId, PluginPaths};

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
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("wasmtime: {0}")]
    Wasmtime(#[from] wasmtime::Error),
    #[error("plugin.wasm too large: {size} bytes (max {max})")]
    WasmTooLarge { size: u64, max: u64 },
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
}

impl PluginRuntime {
    /// Build a runtime with the given sandbox config. Fails if
    /// wasmtime rejects the `Config` (impossible with the static
    /// settings here today — defensive against future feature
    /// upgrades).
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
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
        Ok(Self {
            inner: Arc::new(RuntimeInner { engine, config }),
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
        let wasm_path = paths.wasm_path(plugin_id)?;
        // Stat first — refuse to slurp a multi-GB blob into a `Vec<u8>`
        // even if the user (or a malicious sideload) put one in the
        // plugin directory. `fs::metadata` is cheap and lets us
        // surface the size in the error without paying the read cost.
        let size = std::fs::metadata(&wasm_path)?.len();
        if size > MAX_WASM_SIZE {
            return Err(RuntimeError::WasmTooLarge {
                size,
                max: MAX_WASM_SIZE,
            });
        }
        let bytes = std::fs::read(&wasm_path)?;
        let component = Component::from_binary(&self.inner.engine, &bytes)?;
        Ok(LoadedPlugin {
            manifest,
            component,
            wasm_path,
        })
    }

    /// Construct a fresh [`Store`] for one plugin invocation. The
    /// store is preconfigured with this runtime's fuel + epoch
    /// deadlines + memory cap — callers don't need to remember to
    /// apply them. The `data` field on the returned store is the
    /// host context (Phase 2b will substitute the real `HostCtx`
    /// once the import traits are wired).
    pub fn new_store(&self) -> Result<Store<HostCtx>, RuntimeError> {
        let ctx = HostCtx::new(self.inner.config.max_memory_bytes);
        let mut store = Store::new(&self.inner.engine, ctx);

        store.set_fuel(self.inner.config.fuel_per_call)?;
        // `set_epoch_deadline` is in epoch-tick units; the host
        // increments the engine's epoch counter and the store
        // traps when it crosses the deadline.
        store.set_epoch_deadline(self.inner.config.epoch_ticks_per_call);
        // Apply the memory limiter we configured on `HostCtx::new`.
        store.limiter(|ctx| &mut ctx.limits);

        Ok(store)
    }
}

/// One plugin loaded from disk: parsed manifest + compiled
/// component + the path the wasm came from (kept for diagnostics).
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub component: Component,
    pub wasm_path: PathBuf,
}

/// Per-Store host context. Phase 2a only carries the resource
/// table + memory limiter so the runtime can already enforce its
/// sandbox; Phase 2b adds the HTTP allowlist, log scope (plugin id),
/// scratch-store handle, and any other state the host imports need
/// at call time.
///
/// Why a struct vs `()`: the `bindgen!`-generated `Host` trait will
/// be implemented on `HostCtx` (not on a generic), so introducing
/// the type now keeps Phase 2b's diff small + reviewable.
pub struct HostCtx {
    /// Wasmtime resource table — required by `bindgen!` even when
    /// the WIT files don't declare resources (the macro stitches it
    /// into the generated `add_to_linker` boilerplate).
    pub table: ResourceTable,
    /// Memory cap enforcement. The [`Store::limiter`] closure points
    /// at this field so a guest `memory.grow` over the cap returns
    /// 0 instead of asking the OS for more pages.
    pub limits: StoreLimits,
}

impl HostCtx {
    pub fn new(max_memory_bytes: usize) -> Self {
        let limits = StoreLimitsBuilder::new()
            .memory_size(max_memory_bytes)
            .build();
        Self {
            table: ResourceTable::new(),
            limits,
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
        let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine builds");
        let store = runtime.new_store().expect("store builds");
        // Fuel is set; reading it back should match the config.
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
