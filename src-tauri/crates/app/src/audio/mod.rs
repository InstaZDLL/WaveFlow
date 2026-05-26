//! Audio playback subsystem.
//!
//! Three-thread architecture:
//! - Tauri command handlers (tokio) send `AudioCmd`s on a crossbeam channel
//! - A dedicated `std::thread` owns the symphonia decoder state
//! - cpal runs a real-time callback that pulls f32 samples from an SPSC ring
//!
//! Shared state lives in `SharedPlayback` — every field is atomic so the
//! audio callback can read/write without locks.
//!
//! This module is built up incrementally across the audio playback
//! checkpoints; `#[allow(dead_code)]` on currently-unwired items keeps
//! cargo quiet until the command layer comes online.

#![allow(dead_code)]

pub mod analytics;
pub mod crossfade;
pub mod decoder;
pub mod dsd;
pub mod engine;
pub mod eq;
pub mod output;
pub mod resampler;
pub mod spectrum;
pub mod state;
#[cfg(target_os = "windows")]
pub mod wasapi_exclusive;

pub use engine::{AudioCmd, AudioEngine};
pub use output::list_output_devices;
pub use state::PlayerState;
