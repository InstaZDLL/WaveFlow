//! # waveflow-core
//!
//! Business logic shared between the WaveFlow desktop app (`crates/app`,
//! Tauri 2) and the future `waveflow-server` (axum, RFC-001 §6.2).
//!
//! See [`docs/architecture/crates.md`](../../../../docs/architecture/crates.md)
//! for the split rules. In short: anything that could run inside an axum
//! handler (parsing, metadata enrichment, DSD conversion, repository
//! traits, smart-playlist algorithms) belongs here; anything coupled to
//! the Tauri runtime or to the real-time `cpal` audio engine stays in
//! `crates/app`.

pub mod analysis;
pub mod artwork;
pub mod audio_format;
pub mod domain;
pub mod error;
pub mod metadata;
// `plugin` carries the wasmtime + Cranelift + WASI stack (~5 MiB
// of native codegen). Gated so `waveflow-server` (which never
// executes guest WASM in v1) can opt out and stay lean. The
// desktop app turns the feature on via its
// `waveflow-core = { ..., features = ["sqlite", "plugins"] }`
// path-dep.
#[cfg(feature = "plugins")]
pub mod plugin;
pub mod repository;
pub mod scanner;
pub mod smart_playlists;
pub mod sync;
