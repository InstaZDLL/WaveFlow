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
pub mod decoder;
pub mod engine;
pub mod output;
pub mod resampler;
pub mod state;

pub use engine::{AudioCmd, AudioEngine};
