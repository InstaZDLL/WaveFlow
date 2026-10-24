//! Persistence for DLNA settings.
//!
//! Stored in `app_setting` (the global key-value table) rather than
//! `profile_setting` because the server lives at the process level,
//! not per profile — switching profiles re-binds the same listener
//! to whatever the new profile points at, but the config is shared.
//!
//! Keys:
//!   - `dlna.enabled`     — `"1"` / `"0"`, default `"0"` (off until
//!     the user opts in; broadcasting on every launch is unexpected).
//!   - `dlna.server_name` — friendly name shown in controllers,
//!     default `"WaveFlow"`.
//!   - `dlna.port`        — TCP port for HTTP. `0` (default) lets the
//!     OS pick a free port; controllers don't care because the SSDP
//!     LOCATION header carries the actual port. Users can pin a port
//!     if their firewall is configured for it.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppResult;

const KEY_ENABLED: &str = "dlna.enabled";
const KEY_NAME: &str = "dlna.server_name";
const KEY_PORT: &str = "dlna.port";

const DEFAULT_NAME: &str = "WaveFlow";
const DEFAULT_PORT: u16 = 0;

/// Persisted server configuration. Cheap to clone — only carries the
/// three settings the user can actually edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DlnaConfig {
    pub enabled: bool,
    pub server_name: String,
    pub port: u16,
}

impl Default for DlnaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server_name: DEFAULT_NAME.into(),
            port: DEFAULT_PORT,
        }
    }
}

/// Read the full config out of `app_setting`. Missing keys fall back
/// to defaults so the table doesn't need a migration to seed values.
pub async fn load(pool: &SqlitePool) -> AppResult<DlnaConfig> {
    let mut cfg = DlnaConfig::default();
    if let Some(v) = read_key(pool, KEY_ENABLED).await? {
        cfg.enabled = v == "1";
    }
    if let Some(v) = read_key(pool, KEY_NAME).await? {
        if !v.trim().is_empty() {
            cfg.server_name = v;
        }
    }
    if let Some(v) = read_key(pool, KEY_PORT).await? {
        if let Ok(p) = v.parse::<u16>() {
            cfg.port = p;
        }
    }
    Ok(cfg)
}

pub async fn save(pool: &SqlitePool, cfg: &DlnaConfig) -> AppResult<()> {
    write_key(pool, KEY_ENABLED, if cfg.enabled { "1" } else { "0" }).await?;
    write_key(pool, KEY_NAME, &cfg.server_name).await?;
    write_key(pool, KEY_PORT, &cfg.port.to_string()).await?;
    Ok(())
}

async fn read_key(pool: &SqlitePool, key: &str) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(value)
}

async fn write_key(pool: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    // `app_setting.value_type` + `updated_at` are NOT NULL with no default.
    // SQLite checks them on the row an upsert *would* insert, before it
    // looks for a conflict, so leaving them out failed every save — an
    // existing key as much as a new one — and the Settings toggle never
    // stuck. Everything here is stored as text and parsed back on read,
    // so `'string'` is the honest type.
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    /// `app.db` as the app opens it: the real migrator, foreign keys on.
    /// One connection, since every `:memory:` connection is its own database.
    async fn app_db() -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/app")
            .run(&pool)
            .await
            .unwrap();
        pool
    }

    /// The first save on a fresh database inserts every key; it used to
    /// fail on `app_setting.value_type`, so turning the server on from
    /// Settings never persisted.
    #[tokio::test]
    async fn a_first_save_persists_every_key() {
        let pool = app_db().await;
        assert_eq!(load(&pool).await.unwrap(), DlnaConfig::default());

        let cfg = DlnaConfig {
            enabled: true,
            server_name: "Living room".into(),
            port: 18200,
        };
        save(&pool, &cfg).await.unwrap();
        assert_eq!(load(&pool).await.unwrap(), cfg);

        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT key, value_type, updated_at FROM app_setting
             WHERE key LIKE 'dlna.%' ORDER BY key",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        for (key, value_type, updated_at) in rows {
            assert_eq!(value_type, "string", "{key}");
            assert!(updated_at > 0, "{key}");
        }
    }

    /// Saving over keys that already exist takes the upsert's update path,
    /// which the same NOT NULL check also guarded.
    #[tokio::test]
    async fn a_second_save_overwrites_the_first() {
        let pool = app_db().await;
        let on = DlnaConfig {
            enabled: true,
            server_name: "Den".into(),
            port: 0,
        };
        save(&pool, &on).await.unwrap();
        let off = DlnaConfig {
            enabled: false,
            ..on.clone()
        };
        save(&pool, &off).await.unwrap();
        assert_eq!(load(&pool).await.unwrap(), off);
    }
}
