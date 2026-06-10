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
    // matches what `web-radio/src/lib.rs` ships exactly. A
    // round-trip through bindgen → wasm → host imports → guest
    // exports + a faithful entry list together prove the full
    // SDK end-to-end. The expected vector mirrors the plugin's
    // hardcoded catalogue; any drift (add / remove / rename /
    // reorder) trips the equality check so the test fails loudly
    // on silent regressions.
    let entries = plugin
        .waveflow_source_provider()
        .call_list_entries(&mut store)
        .expect("list_entries call")
        .expect("list_entries Ok");

    let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
    let expected: Vec<&str> = vec![
        "Top stations",
        "Trending now",
        "Jazz",
        "Rock",
        "Pop",
        "Electronic",
        "Classical",
        "News",
        "Hip-Hop",
        "Country",
        "Lofi",
        "Ambient",
    ];
    assert_eq!(
        labels, expected,
        "category list mismatch — refresh the fixture and update the expected vector if intentional"
    );

    // Every category MUST carry a non-empty opaque `query` token —
    // an empty value would make the host's resolve call a no-op.
    for entry in &entries {
        assert!(
            !entry.query.is_empty(),
            "entry {:?} has empty query",
            entry.label
        );
    }
}
