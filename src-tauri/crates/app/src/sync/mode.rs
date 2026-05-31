//! Per-profile sync mode. Phase 1.f.desktop.3.
//!
//! The active profile carries one of two modes:
//!
//! - [`SyncMode::Local`] — the desktop never enqueues outbound sync
//!   ops. A profile in this mode behaves exactly like a pre-1.f
//!   build: every CRUD command writes to the local SQLite and stops
//!   there. Useful for privacy-conscious users who want WaveFlow as
//!   a pure local player even after signing in to a server (e.g.
//!   maintaining a separate "Kids" profile that doesn't fan out
//!   playlist edits to the parent account).
//!
//! - [`SyncMode::Hybrid`] — the default once a JWT is configured.
//!   Reads stay local (fast); writes go local + the
//!   `sync_pending_op` queue, and the future drain task
//!   (1.f.desktop.4) posts them upstream.
//!
//! ## Why no `Server` mode?
//!
//! RFC-001 originally listed a third "Server-connected" mode where
//! reads come from HTTP instead of the local cache. We're deferring
//! that — `waveflow-web` already covers the thin-client use case,
//! and routing desktop reads through HTTP forfeits the value the
//! local audio engine + file scanner provide. If a real product
//! need surfaces (e.g. an admin browsing a tenant whose library
//! isn't replicated), it lands as a separate `SyncMode::ServerOnly`
//! variant alongside the existing two — the enum is intentionally
//! open-shaped so the persistence + gate logic don't have to change
//! when that day comes.
//!
//! ## Persistence
//!
//! Stored per-profile in `profile_setting['sync.mode']` (TEXT,
//! `value_type='string'`). Per-profile rather than app-wide because
//! the JWT is per-profile too and the two settings have the same
//! conceptual scope — switching profiles swaps both at once.

#![allow(dead_code)]

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppResult;

/// Key used in `profile_setting`. Public so the diagnostic command
/// can SELECT it without a magic string.
pub const KEY: &str = "sync.mode";

/// Per-profile sync mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// No outbound ops. CRUD writes hit the local DB only.
    Local,
    /// Default. Reads local, writes local + queue, drain runs.
    Hybrid,
}

impl SyncMode {
    /// Canonical TEXT representation we round-trip through SQLite.
    /// Keep these stable — once persisted in user profiles, changing
    /// the casing or alias would require a migration.
    pub const fn as_str(self) -> &'static str {
        match self {
            SyncMode::Local => "local",
            SyncMode::Hybrid => "hybrid",
        }
    }

    /// Parse the persisted TEXT back into the enum. Unknown values
    /// fall back to [`SyncMode::Hybrid`] — the sensible default for
    /// a profile that already has a JWT row but somehow ended up
    /// with a corrupt setting (e.g. a future build introduced a new
    /// mode and an older build is reading it).
    pub fn from_storage(raw: &str) -> Self {
        match raw.trim() {
            "local" => SyncMode::Local,
            // Hybrid is the default for both the literal "hybrid"
            // string and any unknown value — see the doc comment.
            _ => SyncMode::Hybrid,
        }
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Read the mode for the active profile. Returns [`SyncMode::Hybrid`]
/// when no row has been written yet — that's the sensible default for
/// a profile that has just signed in (the [`crate::sync::hooks`]
/// gate combines this with a JWT-presence check, so a fresh
/// unconfigured profile still doesn't enqueue anything).
pub async fn read(profile_pool: &SqlitePool) -> AppResult<SyncMode> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
        .bind(KEY)
        .fetch_optional(profile_pool)
        .await?;
    Ok(raw
        .map(|s| SyncMode::from_storage(&s))
        .unwrap_or(SyncMode::Hybrid))
}

/// Persist the mode for the active profile. Caller is responsible
/// for invalidating any sync-mode-derived UI state — the helper only
/// touches the row.
pub async fn write(profile_pool: &SqlitePool, mode: SyncMode) -> AppResult<()> {
    sqlx::query(
        // ON CONFLICT also touches `value_type` even though
        // `mode::write` is the only writer for this key and always
        // inserts `'string'` — keeps the row internally coherent
        // against a hypothetical future writer that mistakenly used
        // a different type. Cheap defence in depth.
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value,
                value_type = excluded.value_type,
                updated_at = excluded.updated_at",
    )
    .bind(KEY)
    .bind(mode.as_str())
    .bind(now_ms())
    .execute(profile_pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE profile_setting (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                value_type TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn read_defaults_to_hybrid_on_fresh_profile() {
        let pool = pool().await;
        assert_eq!(read(&pool).await.unwrap(), SyncMode::Hybrid);
    }

    #[tokio::test]
    async fn round_trip_local() {
        let pool = pool().await;
        write(&pool, SyncMode::Local).await.unwrap();
        assert_eq!(read(&pool).await.unwrap(), SyncMode::Local);
    }

    #[tokio::test]
    async fn round_trip_hybrid_overwrites_local() {
        let pool = pool().await;
        write(&pool, SyncMode::Local).await.unwrap();
        write(&pool, SyncMode::Hybrid).await.unwrap();
        assert_eq!(read(&pool).await.unwrap(), SyncMode::Hybrid);
    }

    #[tokio::test]
    async fn unknown_storage_value_falls_back_to_hybrid() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES (?, 'future-mode-name', 'string', 0)",
        )
        .bind(KEY)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(read(&pool).await.unwrap(), SyncMode::Hybrid);
    }

    #[test]
    fn as_str_round_trips_through_from_storage() {
        for mode in [SyncMode::Local, SyncMode::Hybrid] {
            assert_eq!(SyncMode::from_storage(mode.as_str()), mode);
        }
    }
}
