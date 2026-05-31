//! Tauri commands exposed to the frontend.
//!
//! Commands are grouped by domain. Each submodule declares the types shared
//! with the frontend via `serde`, plus the `#[tauri::command]` entry points.

pub mod analysis;
pub mod app_info;
pub mod backup;
pub mod browse;
pub mod changelog;
pub mod deezer;
pub mod diagnostics;
pub mod dlna;
pub mod duplicates;
pub mod edit;
pub mod integration;
pub mod library;
pub mod lyrics;
pub mod maintenance;
pub mod mood_radio;
pub mod offline;
pub mod player;
pub mod playlist;
pub mod playlist_cover;
pub mod preferences;
pub mod profile;
pub mod profile_io;
pub mod radio;
pub mod scan;
pub mod server_auth;
pub mod share;
pub mod similar;
pub mod smart_playlists;
pub mod spotify;
pub mod stats;
pub mod track;
pub mod tray;
pub mod wrapped;
