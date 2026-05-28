//! Profile DTOs. A profile is a sandboxed library (its own `data.db`,
//! its own artwork dir, its own scrobbler credentials). The desktop
//! app exposes a Netflix-style selector that switches between them.

use serde::{Deserialize, Serialize};

/// Mirrors the `profile` table in `app.db`, plus a `data_dir` resolved to
/// an absolute path so the frontend can display it if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlite", derive(sqlx::FromRow))]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub color_id: String,
    pub avatar_hash: Option<String>,
    pub data_dir: String,
    pub created_at: i64,
    pub last_used_at: i64,
}

/// Input payload for the `create_profile` Tauri command (and the future
/// `POST /profiles` REST endpoint).
#[derive(Debug, Deserialize)]
pub struct CreateProfileInput {
    pub name: String,
    pub color_id: Option<String>,
    pub avatar_hash: Option<String>,
}
