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
