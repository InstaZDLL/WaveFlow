use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use std::sync::Arc;

use crate::{
    audio::{AudioCmd, AudioEngine},
    error::{AppError, AppResult},
    paths::AppPaths,
    state::AppState,
};

/// Profile row returned to the frontend.
///
/// Mirrors the `profile` table in `app.db`, plus a `data_dir` resolved to an
/// absolute path so the frontend can display it if needed.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub color_id: String,
    pub avatar_hash: Option<String>,
    pub data_dir: String,
    pub created_at: i64,
    pub last_used_at: i64,
}

/// Input payload for [`create_profile`].
#[derive(Debug, Deserialize)]
pub struct CreateProfileInput {
    pub name: String,
    pub color_id: Option<String>,
    pub avatar_hash: Option<String>,
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// List every profile registered in `app.db`, most-recently-used first.
#[tauri::command]
pub async fn list_profiles(state: tauri::State<'_, AppState>) -> AppResult<Vec<Profile>> {
    let profiles = sqlx::query_as::<_, Profile>(
        "SELECT id, name, color_id, avatar_hash, data_dir, created_at, last_used_at
           FROM profile
          ORDER BY last_used_at DESC",
    )
    .fetch_all(&state.app_db)
    .await?;

    Ok(profiles)
}

/// Return the currently active profile (the one whose `data.db` is opened),
/// or `None` if no profile has been activated yet.
#[tauri::command]
pub async fn get_active_profile(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<Profile>> {
    let Some(profile_id) = ({
        let guard = state.profile.read().await;
        guard.as_ref().map(|p| p.profile_id)
    }) else {
        return Ok(None);
    };

    let profile = sqlx::query_as::<_, Profile>(
        "SELECT id, name, color_id, avatar_hash, data_dir, created_at, last_used_at
           FROM profile WHERE id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&state.app_db)
    .await?;

    Ok(profile)
}

/// Create a new profile.
///
/// Steps:
/// 1. Insert the profile row in `app.db`.
/// 2. Materialize `profiles/<id>/` and `profiles/<id>/artwork/`.
/// 3. Open `profiles/<id>/data.db` once to run the initial migration, then
///    close it immediately (the caller can switch to it later via
///    [`switch_profile`]).
#[tauri::command]
pub async fn create_profile(
    state: tauri::State<'_, AppState>,
    input: CreateProfileInput,
) -> AppResult<Profile> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("profile name cannot be empty".into()));
    }
    let color_id = input.color_id.unwrap_or_else(|| "emerald".to_string());
    let now = now_millis();

    // Reserve the row so we get a stable id for the data directory.
    let insert = sqlx::query(
        "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
         VALUES (?, ?, ?, '', ?, ?)",
    )
    .bind(&name)
    .bind(&color_id)
    .bind(input.avatar_hash.as_deref())
    .bind(now)
    .bind(now)
    .execute(&state.app_db)
    .await?;

    let profile_id = insert.last_insert_rowid();
    let rel_dir = AppPaths::profile_rel_dir(profile_id);

    sqlx::query("UPDATE profile SET data_dir = ? WHERE id = ?")
        .bind(&rel_dir)
        .bind(profile_id)
        .execute(&state.app_db)
        .await?;

    // Materialize the filesystem layout and initialize the per-profile DB.
    state.paths.ensure_profile_dirs(profile_id)?;
    let pool = crate::db::profile_db::open(
        &state.paths.profile_db(profile_id),
        &state.paths.app_db,
    )
    .await?;
    pool.close().await;

    Ok(Profile {
        id: profile_id,
        name,
        color_id,
        avatar_hash: input.avatar_hash,
        data_dir: rel_dir,
        created_at: now,
        last_used_at: now,
    })
}

/// Switch the active profile. Closes the current profile pool (if any),
/// opens the target profile's pool, and records it as `app.last_profile_id`
/// so the next startup can restore it.
#[tauri::command]
pub async fn switch_profile(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    watcher: tauri::State<'_, Arc<crate::watcher::WatcherManager>>,
    profile_id: i64,
) -> AppResult<Profile> {
    let profile = sqlx::query_as::<_, Profile>(
        "SELECT id, name, color_id, avatar_hash, data_dir, created_at, last_used_at
           FROM profile WHERE id = ?",
    )
    .bind(profile_id)
    .fetch_optional(&state.app_db)
    .await?
    .ok_or(AppError::ProfileNotFound(profile_id))?;

    // Stop playback before swapping the pool — the queue references
    // track IDs from the old profile's database, which would become
    // dangling after the pool swap.
    let _ = engine.send(AudioCmd::Stop);
    engine.shared().set_state(crate::audio::state::PlayerState::Idle);
    engine
        .shared()
        .current_track_id
        .store(0, std::sync::atomic::Ordering::Release);

    // Stop the previous profile's watchers before swapping pools so
    // their next scan can't race a stale pool reference.
    watcher.unwatch_all();

    state.activate_profile(profile_id).await?;

    // Re-arm watchers from the new profile's library_folder rows.
    if let Ok(pool) = state.require_profile_pool().await {
        if let Err(err) = watcher.restore_from_db(&pool).await {
            tracing::warn!(%err, "watcher restore after profile switch failed");
        }
    }

    let now = now_millis();

    sqlx::query("UPDATE profile SET last_used_at = ? WHERE id = ?")
        .bind(now)
        .bind(profile_id)
        .execute(&state.app_db)
        .await?;

    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES ('app.last_profile_id', ?, 'int', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(profile_id.to_string())
    .bind(now)
    .execute(&state.app_db)
    .await?;

    Ok(Profile {
        last_used_at: now,
        ..profile
    })
}

/// Close the active profile without activating a new one. Useful for a
/// "logout" flow.
#[tauri::command]
pub async fn deactivate_profile(
    state: tauri::State<'_, AppState>,
    watcher: tauri::State<'_, Arc<crate::watcher::WatcherManager>>,
) -> AppResult<()> {
    watcher.unwatch_all();
    state.deactivate_profile().await;
    Ok(())
}
