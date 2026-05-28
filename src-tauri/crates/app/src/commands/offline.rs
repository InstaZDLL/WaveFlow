//! Global offline-mode toggle.
//!
//! When enabled, every command that would otherwise hit Last.fm,
//! Deezer or LRCLIB short-circuits and returns the locally cached
//! result (or an empty payload). The flag is process-wide because
//! offline is a network-stack concern, not a per-profile preference —
//! hopping between profiles shouldn't suddenly re-enable network calls.
//!
//! Persisted in `app_setting['network.offline_mode']` and mirrored on
//! `AppState.offline_mode` (atomic) so hot-path checks are lock-free.

use chrono::Utc;

use crate::{error::AppResult, state::AppState};

const KEY: &str = "network.offline_mode";

#[tauri::command]
pub async fn get_offline_mode() -> AppResult<bool> {
    Ok(crate::offline::is_offline())
}

#[tauri::command]
pub async fn set_offline_mode(state: tauri::State<'_, AppState>, enabled: bool) -> AppResult<()> {
    crate::offline::set(enabled);
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY)
    .bind(if enabled { "true" } else { "false" })
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}
