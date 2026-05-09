//! Frontend-facing entry points for the smart-playlist engine.
//!
//! Kept thin on purpose — the heavy lifting lives in
//! [`crate::smart_playlists::generator`] so it stays unit-testable without a
//! Tauri runtime.

use crate::error::AppResult;
use crate::smart_playlists::generator;
use crate::state::AppState;

/// Regenerate every Daily Mix slot from the active profile's listening
/// history. Returns the playlist ids in slot order so the frontend can
/// optimistically refresh and navigate to the first one.
#[tauri::command]
pub async fn regenerate_daily_mixes(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    generator::regenerate_daily_mixes(&pool, &state.paths).await
}
