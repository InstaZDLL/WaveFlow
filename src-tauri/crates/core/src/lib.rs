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

pub mod domain;
pub mod error;
pub mod metadata;
pub mod repository;
