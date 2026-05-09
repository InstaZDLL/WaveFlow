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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(value)
}

async fn write_key(pool: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_setting (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}
