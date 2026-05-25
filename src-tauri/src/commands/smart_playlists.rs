//! Frontend-facing entry points for the smart-playlist engine.
//!
//! Kept thin on purpose — the heavy lifting lives in
//! [`crate::smart_playlists::generator`] so it stays unit-testable without a
//! Tauri runtime.

use crate::error::{AppError, AppResult};
use crate::smart_playlists::{
    custom::{self, CustomRules},
    generator, on_repeat, SmartPlaylistRules,
};
use crate::state::AppState;
use serde::Deserialize;
use tauri::{AppHandle, Emitter};

/// Regenerate every Daily Mix slot from the active profile's listening
/// history. Returns the playlist ids in slot order so the frontend can
/// optimistically refresh and navigate to the first one.
#[tauri::command]
pub async fn regenerate_daily_mixes(state: tauri::State<'_, AppState>) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    generator::regenerate_daily_mixes(&pool, &state.paths, profile_id).await
}

/// Regenerate the active profile's On Repeat playlist from the last
/// 30 days of listening history. Returns the playlist id, or `null`
/// when there's not enough data to materialize anything (the previous
/// row, if any, is cleaned up in that case).
#[tauri::command]
pub async fn regenerate_on_repeat(state: tauri::State<'_, AppState>) -> AppResult<Option<i64>> {
    let pool = state.require_profile_pool().await?;
    on_repeat::regenerate_on_repeat(&pool, &state.paths).await
}

/// One-shot regen for every built-in smart-playlist family (Daily Mix
/// slots + On Repeat). The frontend's "Régénérer" button calls this so
/// the user gets the whole "Made for you" surface refreshed in a single
/// click without having to know about the family split.
#[derive(Debug, serde::Serialize)]
pub struct RegenerateAllSmartPlaylistsOutput {
    pub daily_mix_ids: Vec<i64>,
    pub on_repeat_id: Option<i64>,
}

#[tauri::command]
pub async fn regenerate_all_smart_playlists(
    state: tauri::State<'_, AppState>,
) -> AppResult<RegenerateAllSmartPlaylistsOutput> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let daily_mix_ids = generator::regenerate_daily_mixes(&pool, &state.paths, profile_id).await?;
    let on_repeat_id = on_repeat::regenerate_on_repeat(&pool, &state.paths).await?;
    Ok(RegenerateAllSmartPlaylistsOutput {
        daily_mix_ids,
        on_repeat_id,
    })
}

#[derive(Debug, Deserialize)]
pub struct CustomSmartPlaylistInput {
    pub name: String,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
    pub rules: CustomRules,
}

/// Create a new custom smart playlist + materialize its tracks. The
/// playlist row is persisted with `is_smart = 1` and the rule set
/// stored in `playlist.smart_rules` so [`regenerate_custom_smart_playlist`]
/// can re-run it later. Returns the new playlist id and the count of
/// tracks materialized.
#[derive(Debug, serde::Serialize)]
pub struct CustomSmartPlaylistOutput {
    pub playlist_id: i64,
    pub track_count: i64,
}

#[tauri::command]
pub async fn create_custom_smart_playlist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    input: CustomSmartPlaylistInput,
) -> AppResult<CustomSmartPlaylistOutput> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("playlist name cannot be empty".into()));
    }
    let pool = state.require_profile_pool().await?;
    let now = chrono::Utc::now().timestamp_millis();
    let color_id = input.color_id.unwrap_or_else(|| "violet".to_string());
    let icon_id = input.icon_id.unwrap_or_else(|| "sparkles".to_string());
    let rules_json = SmartPlaylistRules::Custom {
        rules: input.rules.clone(),
    }
    .to_json();

    let insert = sqlx::query(
        "INSERT INTO playlist
             (name, description, color_id, icon_id, is_smart, smart_rules,
              position, created_at, updated_at)
         VALUES (?, ?, ?, ?, 1, ?, 0, ?, ?)",
    )
    .bind(&name)
    .bind(input.description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(&rules_json)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await?;
    let playlist_id = insert.last_insert_rowid();

    let track_count = custom::materialize(&pool, playlist_id, &input.rules).await?;
    let _ = app.emit("playlist:changed", playlist_id);

    Ok(CustomSmartPlaylistOutput {
        playlist_id,
        track_count,
    })
}

/// Update the rule set of an existing custom smart playlist and
/// re-materialize. The playlist must already be `is_smart = 1` with a
/// `Custom { ... }` rules payload — calling this on a Daily Mix or a
/// manual playlist returns an error.
#[tauri::command]
pub async fn update_custom_smart_playlist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    input: CustomSmartPlaylistInput,
) -> AppResult<CustomSmartPlaylistOutput> {
    let pool = state.require_profile_pool().await?;
    let row: Option<(i64, Option<String>)> =
        sqlx::query_as("SELECT is_smart, smart_rules FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_optional(&pool)
            .await?;
    let (is_smart, existing_rules) =
        row.ok_or_else(|| AppError::Other(format!("playlist {playlist_id} not found")))?;
    if is_smart != 1 {
        return Err(AppError::Other("playlist is not a smart playlist".into()));
    }
    if !is_custom_payload(existing_rules.as_deref()) {
        return Err(AppError::Other(
            "playlist is a built-in smart playlist (Daily Mix), not a custom one".into(),
        ));
    }

    let now = chrono::Utc::now().timestamp_millis();
    let rules_json = SmartPlaylistRules::Custom {
        rules: input.rules.clone(),
    }
    .to_json();
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("playlist name cannot be empty".into()));
    }
    let color_id = input.color_id.unwrap_or_else(|| "violet".to_string());
    let icon_id = input.icon_id.unwrap_or_else(|| "sparkles".to_string());

    sqlx::query(
        "UPDATE playlist
            SET name = ?, description = ?, color_id = ?, icon_id = ?,
                smart_rules = ?, updated_at = ?
          WHERE id = ?",
    )
    .bind(&name)
    .bind(input.description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(&rules_json)
    .bind(now)
    .bind(playlist_id)
    .execute(&pool)
    .await?;

    let track_count = custom::materialize(&pool, playlist_id, &input.rules).await?;
    let _ = app.emit("playlist:changed", playlist_id);

    Ok(CustomSmartPlaylistOutput {
        playlist_id,
        track_count,
    })
}

/// Re-materialize an existing custom smart playlist from its stored
/// rules. Useful after the library changes (new files imported, tracks
/// re-tagged) so the membership picks up the new rows without forcing
/// the user to re-open the rule editor.
#[tauri::command]
pub async fn regenerate_custom_smart_playlist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<CustomSmartPlaylistOutput> {
    let pool = state.require_profile_pool().await?;
    let row: Option<(i64, Option<String>)> =
        sqlx::query_as("SELECT is_smart, smart_rules FROM playlist WHERE id = ?")
            .bind(playlist_id)
            .fetch_optional(&pool)
            .await?;
    let (is_smart, raw) =
        row.ok_or_else(|| AppError::Other(format!("playlist {playlist_id} not found")))?;
    if is_smart != 1 {
        return Err(AppError::Other("playlist is not a smart playlist".into()));
    }
    let rules = parse_custom_rules(raw.as_deref())?;
    let track_count = custom::materialize(&pool, playlist_id, &rules).await?;
    let _ = app.emit("playlist:changed", playlist_id);
    Ok(CustomSmartPlaylistOutput {
        playlist_id,
        track_count,
    })
}

/// Read the rule set of a custom smart playlist for the editor's
/// "Edit" path. Errors when the playlist isn't a Custom one.
#[tauri::command]
pub async fn get_custom_smart_playlist_rules(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<CustomRules> {
    let pool = state.require_profile_pool().await?;
    let raw: Option<Option<String>> =
        sqlx::query_scalar("SELECT smart_rules FROM playlist WHERE id = ? AND is_smart = 1")
            .bind(playlist_id)
            .fetch_optional(&pool)
            .await?;
    let raw = raw
        .flatten()
        .ok_or_else(|| AppError::Other(format!("smart playlist {playlist_id} not found")))?;
    parse_custom_rules(Some(&raw))
}

/// Run the rule set against the current library without persisting
/// anything. Powers the "Preview" button in the rule editor — returns
/// the matched track count plus the first 200 ids so the UI can show
/// a preview list.
#[derive(Debug, serde::Serialize)]
pub struct RulesPreview {
    pub total: i64,
    pub track_ids: Vec<i64>,
}

#[tauri::command]
pub async fn preview_custom_smart_playlist(
    state: tauri::State<'_, AppState>,
    rules: CustomRules,
) -> AppResult<RulesPreview> {
    let pool = state.require_profile_pool().await?;
    let ids = custom::run_query(&pool, &rules).await?;
    let preview: Vec<i64> = ids.iter().take(200).copied().collect();
    Ok(RulesPreview {
        total: ids.len() as i64,
        track_ids: preview,
    })
}

fn is_custom_payload(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    matches!(
        serde_json::from_str::<SmartPlaylistRules>(raw),
        Ok(SmartPlaylistRules::Custom { .. })
    )
}

fn parse_custom_rules(raw: Option<&str>) -> AppResult<CustomRules> {
    let raw = raw.ok_or_else(|| AppError::Other("smart_rules column is empty".into()))?;
    match serde_json::from_str::<SmartPlaylistRules>(raw) {
        Ok(SmartPlaylistRules::Custom { rules }) => Ok(rules),
        Ok(_) => Err(AppError::Other(
            "playlist is a built-in smart playlist (Daily Mix), not a custom one".into(),
        )),
        Err(e) => Err(AppError::Other(format!("invalid smart_rules JSON: {e}"))),
    }
}
