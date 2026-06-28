//! Wasmtime Component-Model bindings.
//!
//! The [`wasmtime::component::bindgen!`] macro consumes the WIT files
//! under `crates/plugin-sdk/wit/` and emits the host-facing trait
//! shells + the guest-side instantiation glue. Phase 2b binds the
//! `source/v1` world only — the metadata + ui worlds get their own
//! bindgen! modules when their first plugins land (Phase 3/5).
//!
//! Why scope it to one world right now: each `bindgen!` invocation
//! compiles a parallel copy of all the import traits, so binding
//! all three worlds in one shot triples the generated-code surface
//! before any of them has a consumer. Phase 4 (Web Radio = source)
//! is the only plugin v1.5.0 actually ships, so we wire the world
//! it needs and leave the others to follow real demand.
//!
//! The generated trait we'll implement on [`HostCtx`](crate::plugin::runtime::HostCtx)
//! lives under [`source::waveflow::host`]; see [`crate::plugin::host_impl`]
//! for the concrete impls that gate HTTP / storage / logging
//! through the manifest permissions.

/// `waveflow:source/plugin@1.0.0` — the WIT world Web Radio (and
/// every future source-provider plugin) exports. Generated module
/// hierarchy mirrors the WIT package namespace:
///
/// - `Plugin` — the world-level struct with `instantiate` +
///   `provider()` accessors for the exported interface.
/// - `exports::waveflow::source::provider::*` — guest call wrappers
///   (`list-entries`, `resolve`, `stream-url`).
/// - `waveflow::host::{http, log, storage}::Host` — the import
///   traits the host must implement on its `Store` data type.
///
/// `trappable_imports: true` lets each Host method return
/// `wasmtime::Result<...>` so a permission denial / quota breach can
/// either bubble back as a recoverable `Err` value the guest sees,
/// or trap the entire instance (Phase 2b uses the former — guests
/// expect a string error per the WIT signatures).
pub mod source {
    wasmtime::component::bindgen!({
        world: "waveflow:source/plugin",
        path: "../plugin-sdk/wit/source",
        imports: { default: trappable },
    });
}

/// `waveflow:ui/plugin@1.0.0` — JSON-descriptor UI extension world.
pub mod ui {
    wasmtime::component::bindgen!({
        world: "waveflow:ui/plugin",
        path: "../plugin-sdk/wit/ui",
        imports: { default: trappable },
    });
}
