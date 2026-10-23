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

pub mod album_playback;
pub mod analysis;
pub mod artwork;
pub mod atomic_write;
pub mod audio_format;
pub mod domain;
pub mod error;
pub mod handoff;
pub mod home_suggestion;
pub mod inventory;
pub mod metadata;
pub mod mood;
// Decoding for playback; the server, which plays nothing, leaves it off.
#[cfg(feature = "playback")]
#[cfg_attr(docsrs, doc(cfg(feature = "playback")))]
pub mod playback;
// `plugin` carries the wasmtime + Cranelift + WASI stack (~5 MiB
// of native codegen). Gated so `waveflow-server` (which never
// executes guest WASM in v1) can opt out and stay lean. The
// desktop app turns the feature on via its
// `waveflow-core = { ..., features = ["sqlite", "plugins"] }`
// path-dep. The `doc(cfg(...))` attribute surfaces the gate in
// rendered docs on `docs.rs` so an external integrator sees the
// requirement without grepping the manifest.
#[cfg(feature = "plugins")]
#[cfg_attr(docsrs, doc(cfg(feature = "plugins")))]
pub mod plugin;
pub mod repository;
pub mod scanner;
pub mod search;
#[cfg(feature = "sqlite")]
pub mod similar;
pub mod smart_playlists;
pub mod sync;
pub mod tagio;
pub mod text_encoding;
