//! Database layer.
//!
//! Two physically separate SQLite databases:
//!
//! * [`app_db`] — global registry of profiles and app-wide settings.
//! * [`profile_db`] — per-profile library, playlists, history, analytics.

pub mod app_db;
pub mod profile_db;
