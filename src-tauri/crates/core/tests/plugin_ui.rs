//! End-to-end test for the `waveflow:ui/v1` world against the
//! committed UI fixture component (`tests/fixtures/ui-fixture/`).
//!
//! Exercises the full loop — `manifest()` → `render(path)` →
//! `on-event(event, payload)` — plus BOTH sides of the redacted
//! `library.read_artists` gate: with the permission the guest sees
//! the (names + counts + opaque ids) snapshot; without it the SAME
//! wasm gets `Err`, proving the redaction is enforced host-side.
//!
//! The fixture wasm + a manifest are checked in so the test stays
//! hermetic — no cargo-component invocation, no wasm32 toolchain at
//! `cargo test` time. Rebuild via `cargo component build --release`
//! in `plugins/ui-fixture/` and refresh the fixture when the guest
//! changes.
//!
//! Gated on the `plugins` feature for the same reason as
//! `plugin_web_radio.rs`: it references the feature-gated
//! `waveflow_core::plugin` module.
#![cfg(feature = "plugins")]

use std::path::PathBuf;

use waveflow_core::plugin::runtime::{
    ui_event, ui_manifest, ui_render, LibraryArtist, PluginRuntime, RuntimeConfig,
};
use waveflow_core::plugin::PluginPaths;

/// Fixture manifest granting the redacted artist read.
const GRANTED_MANIFEST: &str = r#"
schema_version = 1

[plugin]
id = "ui-fixture"
name = "UI Fixture"
version = "0.1.0"
author = "InstaZDLL"
world = "waveflow:ui/v1"

[permissions]
library_read_artists = true
"#;

/// Same fixture, no `library_read_artists` — the host must deny the
/// artist read even though a snapshot is passed in.
const DENIED_MANIFEST: &str = r#"
schema_version = 1

[plugin]
id = "ui-fixture"
name = "UI Fixture"
version = "0.1.0"
author = "InstaZDLL"
world = "waveflow:ui/v1"
"#;

/// Stage the committed fixture wasm under a per-test app-data root,
/// writing `manifest` next to it. Returns the temp dir (kept alive
/// for the test's lifetime) + the resolved paths.
fn stage_fixture(manifest: &str) -> (tempfile::TempDir, PluginPaths) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = PluginPaths::from_app_data(tmp.path());
    let plugin_dir = paths.plugin_dir("ui-fixture").expect("dir");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir");

    let fixture_root: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "ui-fixture"]
        .iter()
        .collect();
    std::fs::copy(
        fixture_root.join("plugin.wasm"),
        plugin_dir.join("plugin.wasm"),
    )
    .expect("copy wasm");
    std::fs::write(plugin_dir.join("manifest.toml"), manifest).expect("write manifest");
    (tmp, paths)
}

/// A tiny redacted snapshot the host would otherwise load from the
/// active profile.
fn artists() -> Vec<LibraryArtist> {
    vec![
        LibraryArtist {
            id: 7,
            name: "Aphex Twin".into(),
            track_count: 42,
        },
        LibraryArtist {
            id: 9,
            name: "Boards of Canada".into(),
            track_count: 17,
        },
    ]
}

#[test]
fn ui_fixture_manifest_returns_mount_point() {
    let (_tmp, paths) = stage_fixture(GRANTED_MANIFEST);
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let mp = ui_manifest(&runtime, &paths, "ui-fixture").expect("manifest");
    assert_eq!(mp.sidebar_label, "UI Fixture");
    assert_eq!(mp.sidebar_icon.as_deref(), Some("radar"));
    assert_eq!(mp.initial_path, "/");
}

#[test]
fn ui_fixture_render_exposes_redacted_artists_when_permitted() {
    let (_tmp, paths) = stage_fixture(GRANTED_MANIFEST);
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let json = ui_render(&runtime, &paths, "ui-fixture", "/", artists()).expect("render");
    let v: serde_json::Value = serde_json::from_str(&json).expect("descriptor is json");

    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["status"], "fresh");
    // The guest echoes the render `path` into the descriptor.
    assert_eq!(v["subtitle"], "path=/");

    let items = v["sections"][0]["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["title"], "Aphex Twin");
    assert_eq!(items[0]["subtitle"], "42 tracks");
    // Redacted read exposes the opaque id — and nothing beyond names
    // + counts + ids.
    assert_eq!(items[0]["id"], "7");
}

#[test]
fn ui_fixture_render_clamps_snapshot_to_host_max() {
    // The fixture requests `u32::MAX` artists; the host must expose at
    // most `MAX_LIBRARY_ARTISTS` regardless of what the guest asks or
    // how large the injected snapshot is. Feed a snapshot bigger than
    // the cap and assert the guest saw exactly the cap.
    use waveflow_core::plugin::host_impl::MAX_LIBRARY_ARTISTS;

    let (_tmp, paths) = stage_fixture(GRANTED_MANIFEST);
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let oversized: Vec<LibraryArtist> = (0..(MAX_LIBRARY_ARTISTS as u64 + 100))
        .map(|i| LibraryArtist {
            id: i,
            name: format!("Artist {i}"),
            track_count: 1,
        })
        .collect();
    let json = ui_render(&runtime, &paths, "ui-fixture", "/", oversized).expect("render");
    let v: serde_json::Value = serde_json::from_str(&json).expect("descriptor is json");

    let items = v["sections"][0]["items"].as_array().expect("items array");
    assert_eq!(
        items.len(),
        MAX_LIBRARY_ARTISTS,
        "host must clamp the exposed artist list to the cap"
    );
}

#[test]
fn ui_fixture_render_denied_without_permission() {
    // Same wasm, a manifest WITHOUT `library_read_artists`. Even
    // though the host passes the snapshot into the store, the
    // permission gate must deny `list-artists` — proving redaction is
    // enforced host-side, not left to the guest's good behaviour.
    let (_tmp, paths) = stage_fixture(DENIED_MANIFEST);
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let json = ui_render(&runtime, &paths, "ui-fixture", "/", artists()).expect("render");
    let v: serde_json::Value = serde_json::from_str(&json).expect("descriptor is json");

    assert_eq!(v["status"], "error");
    let hint = v["emptyHint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("permission denied"),
        "expected a permission-denied hint, got {hint:?}"
    );
    // No artist leaked into the descriptor.
    let items = v["sections"][0]["items"].as_array().expect("items array");
    assert!(items.is_empty(), "denied read must expose no artists");
}

#[test]
fn ui_fixture_on_event_round_trips() {
    let (_tmp, paths) = stage_fixture(GRANTED_MANIFEST);
    let runtime = PluginRuntime::new(RuntimeConfig::default()).expect("engine");
    let json =
        ui_event(&runtime, &paths, "ui-fixture", "refresh", "abc", artists()).expect("on_event");
    let v: serde_json::Value = serde_json::from_str(&json).expect("descriptor is json");
    assert_eq!(v["title"], "event:refresh");
    assert_eq!(v["subtitle"], "payload=abc");
}
