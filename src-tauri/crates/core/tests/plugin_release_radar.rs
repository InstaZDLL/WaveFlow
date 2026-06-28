//! Smoke test for the UI plugin world using the bundled Release Radar
//! component. Keeps the `waveflow:ui/v1` linker/import surface honest.

#![cfg(feature = "plugins")]

use std::path::PathBuf;

use waveflow_core::plugin::runtime::{ui_manifest, ui_render, PluginRuntime, RuntimeConfig};
use waveflow_core::plugin::PluginPaths;

fn stage_fixture() -> (tempfile::TempDir, PluginPaths) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = PluginPaths::from_app_data(tmp.path());
    let plugin_dir = paths.plugin_dir("release-radar").expect("dir");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir");

    let fixture_root: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "resources",
        "plugins",
        "release-radar",
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
fn release_radar_plugin_loads_and_renders_empty_view() {
    let (_tmp, paths) = stage_fixture();
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");

    let mount = ui_manifest(&runtime, &paths, "release-radar").expect("ui manifest");
    assert_eq!(mount.sidebar_label, "Release Radar");
    assert_eq!(mount.initial_path, "/");

    let descriptor =
        ui_render(&runtime, &paths, "release-radar", "/", Vec::new()).expect("ui render");
    assert!(
        descriptor.contains("\"schemaVersion\":1"),
        "descriptor should be camelCase JSON: {descriptor}"
    );
    assert!(
        descriptor.contains("\"title\":\"Release Radar\""),
        "descriptor title missing: {descriptor}"
    );
}
