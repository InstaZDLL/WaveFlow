//! Transitional native host: compile the existing backend sources once per
//! frontend, with a small Rust host boundary. No Tauri, IPC or webview dependency.
//! Shared source paths deliberately prevent diverging copies during extraction.
#![cfg(target_os = "linux")]
#![allow(dead_code)]

#[allow(unused_imports)] // Shared app facade exports Tauri-only device helpers too.
#[path = "../../app/src/audio/mod.rs"]
mod audio;
#[path = "../../app/src/db/mod.rs"]
mod db;
#[path = "../../app/src/offline.rs"]
mod offline;
#[path = "../../app/src/paths.rs"]
mod paths;
#[path = "../../app/src/playback_gain.rs"]
mod playback_gain;
#[path = "../../app/src/player_actions.rs"]
mod player_actions;
#[path = "../../app/src/profile_access.rs"]
mod profile_access;
#[path = "../../app/src/profile_pool.rs"]
mod profile_pool;
#[path = "../../app/src/profile_selection.rs"]
mod profile_selection;
#[path = "../../app/src/queue.rs"]
mod queue;
#[path = "../../app/src/remote_playback.rs"]
mod remote_playback;
#[path = "../../app/src/scrobble_queue.rs"]
mod scrobbler;

pub mod backend;
mod commands;
pub mod error;
pub mod host;
mod state;

pub use audio::PlayerState;
