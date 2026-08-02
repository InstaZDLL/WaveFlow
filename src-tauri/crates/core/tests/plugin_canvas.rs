//! End-to-end test for the `waveflow:canvas/v1` world against the
//! committed Canvas fixture component (`tests/fixtures/canvas-fixture/`).
//!
//! Exercises all three `track-canvas` outcomes the host must handle:
//! a hit (`Ok(Some(canvas))` with the args echoed into the URL +
//! entity-id), a miss (`Ok(None)`), and a provider failure (`Err`).
//!
//! The fixture wasm + a manifest are checked in so the test stays
//! hermetic — no cargo-component invocation, no wasm32 toolchain at
//! `cargo test` time. Rebuild via `cargo component build --release`
//! in `plugins/canvas-fixture/` and refresh the fixture when the guest
//! changes.
//!
//! Gated on the `plugins` feature, like `plugin_ui.rs` /
//! `plugin_web_radio.rs`: it references the feature-gated
//! `waveflow_core::plugin` module.
#![cfg(feature = "plugins")]

use std::path::PathBuf;

use waveflow_core::plugin::runtime::{
    canvas_track_canvas, PluginRuntime, RuntimeConfig, SourceError,
};
use waveflow_core::plugin::PluginPaths;

const MANIFEST: &str = r#"
schema_version = 1

[plugin]
id = "canvas-fixture"
name = "Canvas Fixture"
version = "0.1.0"
author = "InstaZDLL"
world = "waveflow:canvas/v1"
"#;

/// Stage the committed fixture wasm under a per-test app-data root,
/// writing the manifest next to it. Returns the temp dir (kept alive
/// for the test's lifetime) + the resolved paths.
fn stage_fixture() -> (tempfile::TempDir, PluginPaths) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = PluginPaths::from_app_data(tmp.path());
    let plugin_dir = paths.plugin_dir("canvas-fixture").expect("dir");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir");

    let fixture_root: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "canvas-fixture",
    ]
    .iter()
    .collect();
    std::fs::copy(
        fixture_root.join("plugin.wasm"),
        plugin_dir.join("plugin.wasm"),
    )
    .expect("copy wasm");
    std::fs::write(plugin_dir.join("manifest.toml"), MANIFEST).expect("write manifest");
    (tmp, paths)
}

#[test]
fn canvas_fixture_resolves_a_hit_with_args_echoed() {
    let (_tmp, paths) = stage_fixture();
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let got = canvas_track_canvas(
        &runtime,
        &paths,
        "canvas-fixture",
        "Aphex Twin",
        "Xtal",
        Some("Selected Ambient Works"),
        Some(312_000),
    )
    .expect("track_canvas call");
    let canvas = got.expect("a Canvas for a non-empty title");
    assert_eq!(canvas.url, "https://example.com/canvas/Aphex Twin/Xtal.mp4");
    assert_eq!(canvas.entity_id.as_deref(), Some("id:Xtal"));
}

#[test]
fn canvas_fixture_returns_none_for_no_canvas() {
    // Empty title is the fixture's "no Canvas for this track" signal —
    // the host must see `Ok(None)`, not an error.
    let (_tmp, paths) = stage_fixture();
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let got = canvas_track_canvas(&runtime, &paths, "canvas-fixture", "Artist", "", None, None)
        .expect("track_canvas call");
    assert!(got.is_none(), "empty title must resolve to no Canvas");
}

#[test]
fn canvas_fixture_surfaces_a_provider_error() {
    // The fixture returns `Err` for the sentinel title "boom"; the host
    // must surface it as a plugin-level error (which the app-side fanout
    // then logs + skips — fail-soft).
    let (_tmp, paths) = stage_fixture();
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    match canvas_track_canvas(&runtime, &paths, "canvas-fixture", "Artist", "boom", None, None) {
        Err(SourceError::Plugin(msg)) => assert_eq!(msg, "provider failure"),
        Ok(other) => panic!("expected a provider error, got Ok({other:?})"),
        Err(other) => panic!("expected SourceError::Plugin, got {other:?}"),
    }
}
