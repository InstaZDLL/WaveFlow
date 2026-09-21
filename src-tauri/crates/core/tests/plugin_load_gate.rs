//! The host refuses a core module, says which mistake it was, and
//! remembers the refusal until a good binary replaces it.
//!
//! Two published WaveFlow plugins shipped as core modules — `cargo
//! build` where `cargo component build` was meant — and the app said
//! nothing about it anywhere a user would look. These are the
//! behaviours that make it visible, pinned against the same
//! committed component fixture the Canvas test uses, so "a real
//! component still loads" is proven by the same run that proves a
//! module does not.
//!
//! Gated on the `plugins` feature like the other plugin tests.
#![cfg(feature = "plugins")]

use std::path::PathBuf;

use waveflow_core::plugin::binfmt::WasmBinaryKind;
use waveflow_core::plugin::runtime::{PluginRuntime, RuntimeConfig, RuntimeError};
use waveflow_core::plugin::PluginPaths;

const PLUGIN_ID: &str = "canvas-fixture";

const MANIFEST: &str = r#"
schema_version = 1

[plugin]
id = "canvas-fixture"
name = "Canvas Fixture"
version = "0.1.0"
author = "InstaZDLL"
world = "waveflow:canvas/v1"
"#;

/// A core module's whole preamble: the same four magic bytes a
/// component has, then version 1 / layer 0. Nothing follows because
/// nothing needs to — the point of the gate is that the answer is in
/// the first eight bytes, before any parsing.
const CORE_MODULE_PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

fn stage(wasm: &[u8]) -> (tempfile::TempDir, PluginPaths) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = PluginPaths::from_app_data(tmp.path());
    let plugin_dir = paths.plugin_dir(PLUGIN_ID).expect("dir");
    std::fs::create_dir_all(&plugin_dir).expect("mkdir");
    std::fs::write(plugin_dir.join("manifest.toml"), MANIFEST).expect("write manifest");
    std::fs::write(plugin_dir.join("plugin.wasm"), wasm).expect("write wasm");
    (tmp, paths)
}

/// The committed component the Canvas end-to-end test runs against.
fn fixture_component() -> Vec<u8> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "canvas-fixture",
        "plugin.wasm",
    ]
    .iter()
    .collect();
    std::fs::read(path).expect("read fixture wasm")
}

fn runtime() -> PluginRuntime {
    PluginRuntime::new(RuntimeConfig::default()).expect("runtime")
}

/// The refusal has to point at the artifact. "Failed to parse
/// WebAssembly module" — what wasmtime says on its own — sends the
/// reader looking for a corrupt download, which is not what happened
/// and not what fixes it.
#[test]
fn a_core_module_is_refused_by_name() {
    let (_tmp, paths) = stage(&CORE_MODULE_PREAMBLE);
    // `LoadedPlugin` holds a wasmtime `Component` and has no `Debug`,
    // so the success side cannot be unwrapped for a message.
    let Err(err) = runtime().load_plugin(&paths, PLUGIN_ID) else {
        panic!("a core module must not load");
    };

    match &err {
        RuntimeError::NotAComponent { kind } => {
            assert_eq!(*kind, WasmBinaryKind::CoreModule);
        }
        other => panic!("expected NotAComponent, got {other:?}"),
    }
    let message = err.to_string();
    assert!(
        message.contains("cargo component build"),
        "the message must say what to do about it: {message}"
    );
    assert!(
        message.contains("published"),
        "and must point at the artifact, not only at the build command — the two \
         plugins this gate exists for were built correctly and packaged wrong: {message}"
    );
    assert_eq!(err.code(), "not-a-component");
}

/// A load failure is worth showing only if it outlives the call that
/// produced it — the plugin list is drawn long after, by a different
/// command.
#[test]
fn the_refusal_is_remembered_then_dropped_when_the_binary_is_fixed() {
    let (_tmp, paths) = stage(&CORE_MODULE_PREAMBLE);
    let runtime = runtime();

    assert!(
        runtime.load_failure(PLUGIN_ID).is_none(),
        "nothing has been loaded yet"
    );
    assert!(runtime.load_plugin(&paths, PLUGIN_ID).is_err());

    let failure = runtime
        .load_failure(PLUGIN_ID)
        .expect("the refusal must survive the call");
    assert_eq!(failure.code, "not-a-component");
    assert!(failure.detail.contains("cargo component build"));

    // Replace the binary the way an update does, and load again: the
    // plugin is fixed, so nothing must still call it broken.
    let plugin_dir = paths.plugin_dir(PLUGIN_ID).expect("dir");
    std::fs::write(plugin_dir.join("plugin.wasm"), fixture_component()).expect("rewrite wasm");

    runtime
        .load_plugin(&paths, PLUGIN_ID)
        .expect("a real component must load");
    assert!(
        runtime.load_failure(PLUGIN_ID).is_none(),
        "a successful load clears the record"
    );
}

/// The other direction of the same gate: it must not be so eager that
/// it starts refusing the plugins that work. This is the exact binary
/// shipped in the fixture tree, compiled by `cargo component build`.
#[test]
fn a_real_component_still_loads() {
    let (_tmp, paths) = stage(&fixture_component());
    runtime()
        .load_plugin(&paths, PLUGIN_ID)
        .expect("the committed component fixture must load");
}

/// A file that is not wasm at all — an HTML error page saved under the
/// asset's name is the realistic version — must not be reported as a
/// packaging or build mistake, which would send the plugin's author
/// chasing something that is not wrong.
#[test]
fn a_non_wasm_file_is_refused_as_such() {
    let (_tmp, paths) = stage(b"<!DOCTYPE html><title>404</title>");
    let Err(err) = runtime().load_plugin(&paths, PLUGIN_ID) else {
        panic!("a file that is not wasm must not load");
    };
    match &err {
        RuntimeError::NotAComponent { kind } => assert_eq!(*kind, WasmBinaryKind::NotWasm),
        other => panic!("expected NotAComponent, got {other:?}"),
    }
    let message = err.to_string();
    assert!(
        !message.contains("cargo component build"),
        "a file that is not wasm is not a build-command mistake: {message}"
    );
}
