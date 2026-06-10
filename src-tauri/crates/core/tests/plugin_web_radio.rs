//! End-to-end smoke test for the Phase 2/3 plugin SDK against the
//! real Phase 4 Web Radio plugin component.
//!
//! Loads `tests/fixtures/web-radio/{plugin.wasm,manifest.toml}` —
//! both checked into the repo so the test stays hermetic (no
//! cargo-component invocation, no wasm32 toolchain dependency at
//! `cargo test` time). When the plugin code changes, rebuild the
//! .wasm and refresh the fixture.

use std::path::PathBuf;

use waveflow_core::plugin::bindings::source::Plugin;
use waveflow_core::plugin::runtime::{PluginRuntime, RuntimeConfig};
use waveflow_core::plugin::PluginPaths;

/// Stage the Web Radio fixture under a per-test app-data root.
/// Copies the `.wasm` and `manifest.toml` into the install layout
/// `PluginRuntime::load_plugin` expects.
fn stage_fixture() -> (tempfile::TempDir, PluginPaths) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = PluginPaths::from_app_data(tmp.path());
    let plugin_dir = paths.plugin_dir("web-radio").expect("dir");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir");

    let fixture_root: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "web-radio",
    ]
    .iter()
    .collect();
    std::fs::copy(
        fixture_root.join("manifest.toml"),
        plugin_dir.join("manifest.toml"),
    )
    .expect("copy manifest");
    std::fs::copy(
        fixture_root.join("plugin.wasm"),
        plugin_dir.join("plugin.wasm"),
    )
    .expect("copy wasm");
    (tmp, paths)
}

#[test]
fn web_radio_plugin_loads_and_lists_categories() {
    let (_tmp, paths) = stage_fixture();
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let loaded = runtime
        .load_plugin(&paths, "web-radio")
        .expect("load_plugin");
    let linker = runtime.build_linker().expect("linker");
    let mut store = runtime
        .new_store_for_plugin(&loaded, &paths)
        .expect("store");

    // Instantiate the component against the SDK's source-v1 world
    // bindings (the bindgen! macro lives in `plugin::bindings`).
    let plugin =
        Plugin::instantiate(&mut store, &loaded.component, &linker).expect("instantiate");

    // Call the guest's `list-entries` and verify the catalogue
    // matches what `web-radio/src/lib.rs` ships. A non-empty list
    // here proves the full wasmtime → bindgen → host-import →
    // guest export round-trip works end-to-end.
    let entries = plugin
        .waveflow_source_provider()
        .call_list_entries(&mut store)
        .expect("list_entries call")
        .expect("list_entries Ok");

    assert!(
        entries.iter().any(|e| e.label == "Top stations"),
        "Top stations entry should be present: {:?}",
        entries.iter().map(|e| &e.label).collect::<Vec<_>>()
    );
    assert!(
        entries.iter().any(|e| e.label == "Jazz"),
        "Jazz entry should be present"
    );
    assert!(entries.len() >= 10, "expected >= 10 categories");
}
