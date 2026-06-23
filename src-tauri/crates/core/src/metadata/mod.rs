//! HTTP clients for the third-party metadata sources WaveFlow
//! enriches its local library with.
//!
//! Every client here is a plain `reqwest::Client` wrapper — no Tauri,
//! no database, no filesystem. They are designed to be reusable from
//! both the desktop app (`crates/app`) and the future
//! `waveflow-server` (RFC-001 §6.2) without any glue.

pub mod deezer;
pub mod lastfm;
pub mod lrclib;
pub mod theaudiodb;
