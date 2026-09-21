//! Tauri commands exposed to the frontend.
//!
//! Commands are grouped by domain. Each submodule declares the types shared
//! with the frontend via `serde`, plus the `#[tauri::command]` entry points.

pub mod analysis;
pub mod app_info;
pub mod artist_overrides;
pub mod artist_split;
pub mod backup;
pub mod browse;
pub mod canvas;
pub mod changelog;
pub mod deezer;
pub mod desktop_lyrics;
pub mod diagnostics;
pub mod dlna;
pub mod duplicates;
pub mod edit;
pub mod integration;
pub mod library;
pub mod library_media;
// Loopback HTTP listener for the OAuth callback. Its only consumer is
// the server account binding, so it carries the same gate — it used to
// stay ungated because the Spotify handshake needed it in a build
// without sync, and that consumer is gone.
#[cfg(feature = "sync_v2")]
pub mod loopback;
pub mod lyrics;
pub mod lyrics_providers;
pub mod maintenance;
pub mod media_file;
pub mod mood_radio;
pub mod motion_artwork;
pub mod mpd;
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
pub mod ready;
pub mod renderer;
// Remote server binding + PKCE sign-in (RFC-005). Gated on `sync_v2`,
// which is now in the default feature set — a build can still compile it
// out via `--no-default-features`.
pub mod inventory;
#[cfg(feature = "sync_v2")]
pub mod remote_auth;
pub mod scan;
pub mod share_image;
pub mod similar;
pub mod smart_playlists;
pub mod stats;
pub mod storage;
pub mod tag_fetch;
pub mod tasks;
pub mod track;
pub mod track_tags;
pub mod tray;
pub mod updater;
pub mod web_radio_catalogue;
pub mod wrapped;
