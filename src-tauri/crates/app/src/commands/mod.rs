//! Tauri commands exposed to the frontend.
//!
//! Commands are grouped by domain. Each submodule declares the types shared
//! with the frontend via `serde`, plus the `#[tauri::command]` entry points.

pub mod analysis;
pub mod app_info;
pub mod artist_overrides;
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
// Loopback HTTP listener — shared with `commands::spotify` (Spotify
// OAuth handshake), so it's NOT gated alongside the rest of the
// server account binding even though `commands::server_auth` is also
// a consumer. Stays alive whether sync ships or not.
pub mod loopback;
pub mod lyrics;
pub mod maintenance;
pub mod mood_radio;
pub mod motion_artwork;
pub mod offline;
pub mod player;
pub mod playlist;
pub mod playlist_cover;
pub mod plugin_store;
pub mod plugins;
pub mod preferences;
pub mod profile;
pub mod profile_io;
pub mod radio;
pub mod scan;
// Server account binding (Better Auth JWT capture + URL persistence)
// — deferred to 1.6.0. See `sync_stub.rs` / `Cargo.toml`.
#[cfg(feature = "sync_v1")]
pub mod server_auth;
// Public playlist share — depends on the server account binding.
#[cfg(feature = "sync_v1")]
pub mod share;
pub mod similar;
pub mod smart_playlists;
pub mod spotify;
pub mod stats;
// Sync commands (drain, digest, backfill, mode toggle) — deferred to
// 1.6.0. The state.drain / state.ws fields stay alive via the stub
// but their wake calls have no listener.
#[cfg(feature = "sync_v1")]
pub mod sync;
pub mod track;
pub mod tray;
pub mod updater;
pub mod web_radio_catalogue;
pub mod wrapped;
