//! Commands for managing external integration credentials (Last.fm,
//! future: MusicBrainz, etc.) stored in the global `app_setting`
//! table.
//!
//! Keys live in `app_setting` (not per-profile) because an API key is
//! a user-wide concern that shouldn't reset when switching profiles.

use chrono::Utc;

use crate::{error::AppResult, state::AppState};

const LASTFM_KEY: &str = "app.lastfm_api_key";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Return the stored Last.fm API key, or `None` if never configured
/// (or cleared by the user).
#[tauri::command]
pub async fn get_lastfm_api_key(state: tauri::State<'_, AppState>) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(LASTFM_KEY)
    .fetch_optional(&state.app_db)
    .await?;
    Ok(value)
}

/// Upsert the Last.fm API key. Passing an empty string removes the
/// row entirely so the rest of the app treats it as "not configured".
#[tauri::command]
pub async fn set_lastfm_api_key(
    state: tauri::State<'_, AppState>,
    api_key: String,
) -> AppResult<()> {
    let trimmed = api_key.trim();
    if trimmed.is_empty() {
        sqlx::query("DELETE FROM app_setting WHERE key = ?")
            .bind(LASTFM_KEY)
            .execute(&state.app_db)
            .await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(LASTFM_KEY)
    .bind(trimmed)
    .bind(now_ms())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Internal helper used by `enrich_artist_deezer` to look up the key
/// without having to pass it through Tauri's invoke layer.
pub async fn read_lastfm_api_key(state: &AppState) -> AppResult<Option<String>> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(LASTFM_KEY)
    .fetch_optional(&state.app_db)
    .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}
