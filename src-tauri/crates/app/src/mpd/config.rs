//! Persistence for the MPD server settings.
//!
//! Stored in `app_setting` rather than `profile_setting` for the same
//! reason DLNA is (see [`crate::dlna::config`]): the listener lives at
//! the process level, not per profile. Switching profiles re-points the
//! same socket at whatever the new profile holds.
//!
//! Keys:
//!   - `mpd.enabled`  — `"1"` / `"0"`, default `"0"`. This flag **is**
//!     the security decision: enabling the server binds `0.0.0.0`, so
//!     it is off until the user opts in (issue #471).
//!   - `mpd.port`     — TCP port, default `6600` (the MPD standard, and
//!     what every client probes first). Unlike DLNA we don't default to
//!     an OS-assigned port: there is no SSDP here to advertise the real
//!     one, so a random port would mean hand-configuring every client.
//!   - `mpd.password` — optional. Empty means no authentication, which
//!     matches how the DLNA server already behaves on the same LAN.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::AppResult;

const KEY_ENABLED: &str = "mpd.enabled";
const KEY_PORT: &str = "mpd.port";
const KEY_PASSWORD: &str = "mpd.password";

/// The IANA-registered MPD port. Clients default to it, so pinning it
/// means a fresh install needs zero configuration on the client side.
pub const DEFAULT_PORT: u16 = 6600;

/// How far past [`DEFAULT_PORT`] to probe when the configured port is
/// taken. Matches what other MPD implementations do, and keeps a second
/// WaveFlow instance (or an actual mpd daemon) from blocking startup
/// outright.
pub const PORT_SCAN_LEN: u16 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MpdConfig {
    pub enabled: bool,
    pub port: u16,
    /// Empty string = no password required.
    #[serde(default)]
    pub password: String,
}

impl Default for MpdConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            password: String::new(),
        }
    }
}

impl MpdConfig {
    pub fn requires_auth(&self) -> bool {
        !self.password.is_empty()
    }
}

pub async fn load(pool: &SqlitePool) -> AppResult<MpdConfig> {
    let mut cfg = MpdConfig::default();
    if let Some(v) = read_key(pool, KEY_ENABLED).await? {
        cfg.enabled = v == "1";
    }
    if let Some(v) = read_key(pool, KEY_PORT).await? {
        // A stored `0` predates nothing — it just means someone hand-
        // edited the row. Fall back rather than binding a random port
        // no client would find.
        if let Ok(p) = v.parse::<u16>() {
            if p > 0 {
                cfg.port = p;
            }
        }
    }
    if let Some(v) = read_key(pool, KEY_PASSWORD).await? {
        cfg.password = v;
    }
    Ok(cfg)
}

pub async fn save(pool: &SqlitePool, cfg: &MpdConfig) -> AppResult<()> {
    write_key(pool, KEY_ENABLED, if cfg.enabled { "1" } else { "0" }).await?;
    write_key(pool, KEY_PORT, &cfg.port.to_string()).await?;
    write_key(pool, KEY_PASSWORD, &cfg.password).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_on_the_standard_port() {
        let cfg = MpdConfig::default();
        assert!(!cfg.enabled, "the server must stay off until opted into");
        assert_eq!(cfg.port, 6600);
        assert!(!cfg.requires_auth());
    }

    #[test]
    fn a_non_empty_password_turns_auth_on() {
        let cfg = MpdConfig {
            password: "hunter2".into(),
            ..Default::default()
        };
        assert!(cfg.requires_auth());
    }
}
