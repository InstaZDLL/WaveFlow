//! Profile DTOs. A profile is a sandboxed library (its own `data.db`,
//! its own artwork dir, its own scrobbler credentials). The desktop
//! app exposes a Netflix-style selector that switches between them.

use serde::{Deserialize, Serialize};

/// Mirrors the `profile` table in `app.db`, plus a `data_dir` resolved to
/// an absolute path so the frontend can display it if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(any(feature = "sqlite", feature = "postgres"), derive(sqlx::FromRow))]
pub struct Profile {
    pub id: i64,
    /// Owning user id. `0` on single-tenant backends (desktop SQLite
    /// has no `user_id` column on its `profile` table); set to the
    /// row's real owner on multi-tenant Postgres. Sqlx's `#[sqlx(default)]`
    /// makes the field default to `0` when the column is absent from
    /// the SELECT, so the same `Profile` struct round-trips on both
    /// backends without a per-feature shape.
    #[cfg_attr(any(feature = "sqlite", feature = "postgres"), sqlx(default))]
    pub user_id: i64,
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
