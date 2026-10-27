//! A source plugin's favorites, kept per profile.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::*;

/// Key shape for a source plugin's per-profile favorites list in
/// `profile_setting`. Unlike the enabled flag (process-wide,
/// `app_setting`), favorites are user-personal so they live per
/// profile — switching profiles swaps the saved-stations list, like
/// liked tracks.
fn favorites_key(plugin_id: &str) -> String {
    format!("plugin.{plugin_id}.favorites")
}

/// One saved item in a source plugin's favorites list. Mirrors the
/// subset of [`PluginTrack`] the host needs to re-render the row AND
/// replay it without a network round-trip — the `id` already carries
/// the playable token (`url:<stream>` for Web Radio), so
/// [`plugin_stream_url`] resolves it offline. Stored as a JSON array
/// in `profile_setting['plugin.<id>.favorites']`.
///
/// Mirrors Receiver's `favourites.json` (full `Station` objects)
/// rather than storing bare ids: a live API plugin has no stable
/// catalogue to re-hydrate ids against, so the display fields must
/// travel with the favorite.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginFavorite {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub artwork_url: Option<String>,
}

/// Upper bound on stored favorites per plugin. A few hundred covers
/// any realistic "save the stations I like" use; the cap just stops a
/// buggy (or hostile) client from ballooning the `profile_setting`
/// TEXT row without limit. A normal user toggles one star at a time
/// and never approaches it.
const MAX_PLUGIN_FAVORITES: usize = 1000;

/// Read a source plugin's per-profile favorites. Missing row → empty
/// list (a profile that never starred anything). A corrupt row
/// (hand-edited DB, schema drift) is logged + treated as empty rather
/// than failing the whole view — the next [`set_plugin_favorites`]
/// overwrites it cleanly.
#[tauri::command]
pub async fn get_plugin_favorites(
    state: State<'_, AppState>,
    plugin_id: String,
) -> AppResult<Vec<PluginFavorite>> {
    validate_plugin_id_chars(&plugin_id)?;
    let pool = state.require_profile_pool().await?;
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
        .bind(favorites_key(&plugin_id))
        .fetch_optional(&*pool)
        .await?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    match serde_json::from_str::<Vec<PluginFavorite>>(&raw) {
        Ok(list) => Ok(list),
        Err(err) => {
            tracing::warn!(plugin_id, %err, "corrupt plugin favorites row; returning empty");
            Ok(Vec::new())
        }
    }
}

/// Persist a source plugin's per-profile favorites, replacing the
/// whole list (the host owns ordering + dedup; the backend just
/// stores what it's handed). Rejects an over-cap list so a single
/// write can't blow the row size out.
#[tauri::command]
pub async fn set_plugin_favorites(
    state: State<'_, AppState>,
    plugin_id: String,
    favorites: Vec<PluginFavorite>,
) -> AppResult<()> {
    validate_plugin_id_chars(&plugin_id)?;
    if favorites.len() > MAX_PLUGIN_FAVORITES {
        return Err(AppError::Other(format!(
            "too many favorites: {} (max {MAX_PLUGIN_FAVORITES})",
            favorites.len()
        )));
    }
    let pool = state.require_profile_pool().await?;
    let payload = serde_json::to_string(&favorites)
        .map_err(|e| AppError::Other(format!("serialize favorites: {e}")))?;
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'json', ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            value_type = excluded.value_type,
            updated_at = excluded.updated_at",
    )
    .bind(favorites_key(&plugin_id))
    .bind(payload)
    .bind(now)
    .execute(&*pool)
    .await?;
    Ok(())
}
