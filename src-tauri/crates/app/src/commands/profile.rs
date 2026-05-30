use chrono::Utc;

use std::sync::Arc;

use waveflow_core::repository::{
    profile::{ProfileDeleteOutcome, ProfileDraft, ProfileRepository},
    sqlite::SqliteProfileRepository,
};

use crate::{
    audio::{AudioCmd, AudioEngine},
    error::{AppError, AppResult},
    paths::AppPaths,
    state::AppState,
};
// `Profile` + `CreateProfileInput` moved to `waveflow_core::domain::profile`
// in the Phase 1.a refactor. Re-exported so existing call sites
// (`crate::commands::profile::Profile`) keep resolving.
pub use waveflow_core::domain::profile::{CreateProfileInput, Profile};

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

fn profile_repo(state: &AppState) -> SqliteProfileRepository {
    SqliteProfileRepository::new(state.app_db.clone())
}

/// List every profile registered in `app.db`, most-recently-used first.
#[tauri::command]
pub async fn list_profiles(state: tauri::State<'_, AppState>) -> AppResult<Vec<Profile>> {
    Ok(profile_repo(&state).list_all().await?)
}

/// Return the currently active profile (the one whose `data.db` is opened),
/// or `None` if no profile has been activated yet.
#[tauri::command]
pub async fn get_active_profile(state: tauri::State<'_, AppState>) -> AppResult<Option<Profile>> {
    let Some(profile_id) = ({
        let guard = state.profile.read().await;
        guard.as_ref().map(|p| p.profile_id)
    }) else {
        return Ok(None);
    };

    Ok(profile_repo(&state).get(profile_id).await?)
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

    let repo = profile_repo(&state);
    let draft = ProfileDraft {
        name: name.clone(),
        color_id: color_id.clone(),
        avatar_hash: input.avatar_hash.clone(),
        now_ms: now,
    };
    let profile_id = repo.insert(&draft).await?;

    // Wrap every post-insert step that can fail in a single async block,
    // and roll the profile row back if any of them error. Without this,
    // a failure to `mkdir` the profile dir or to open its `data.db`
    // leaves an orphan row in `app.db` that the profile picker would
    // still surface but couldn't activate.
    let rel_dir = AppPaths::profile_rel_dir(profile_id);
    let init = async {
        repo.set_data_dir(profile_id, &rel_dir).await?;
        state.paths.ensure_profile_dirs(profile_id)?;
        let pool =
            crate::db::profile_db::open(&state.paths.profile_db(profile_id), &state.paths.app_db)
                .await?;
        pool.close().await;
        Ok::<(), AppError>(())
    }
    .await;
    if let Err(init_err) = init {
        if let Err(rollback_err) = sqlx::query("DELETE FROM profile WHERE id = ?")
            .bind(profile_id)
            .execute(&state.app_db)
            .await
        {
            tracing::warn!(
                profile_id,
                ?rollback_err,
                "rollback after failed profile init also failed; orphan row left in app.db"
            );
        }
        let _ = std::fs::remove_dir_all(state.paths.profile_dir(profile_id));
        return Err(init_err);
    }

    Ok(Profile {
        id: profile_id,
        // Single-tenant: the desktop's `profile` table has no
        // `user_id` column. The `0` sentinel is what
        // `#[sqlx(default)]` would hand back from a `SELECT` that
        // omits the column anyway, so writing it explicitly here
        // keeps the round-trip consistent.
        user_id: 0,
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
    let repo = profile_repo(&state);
    let profile = repo
        .get(profile_id)
        .await?
        .ok_or(AppError::ProfileNotFound(profile_id))?;

    // Stop playback before swapping the pool — the queue references
    // track IDs from the old profile's database, which would become
    // dangling after the pool swap.
    let _ = engine.send(AudioCmd::Stop);
    engine
        .shared()
        .set_state(crate::audio::state::PlayerState::Idle);
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
    repo.touch_last_used(profile_id, now).await?;

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

/// Rename an existing profile in place. Trims and validates the new
/// name the same way [`create_profile`] does. Used by the onboarding
/// wizard so the auto-created "Default" profile can be renamed
/// without forcing a full create-then-rescan flow, and is safe to
/// call against the active profile (only `app.db` is touched, the
/// per-profile pool is untouched).
#[tauri::command]
pub async fn rename_profile(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
    name: String,
) -> AppResult<Profile> {
    let trimmed = name.trim().to_string();
    if trimmed.is_empty() {
        return Err(AppError::Other("profile name cannot be empty".into()));
    }

    let repo = profile_repo(&state);
    if !repo.rename(profile_id, &trimmed).await? {
        return Err(AppError::ProfileNotFound(profile_id));
    }
    repo.get(profile_id)
        .await?
        .ok_or(AppError::ProfileNotFound(profile_id))
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

/// Permanently delete a profile, its `data.db` and its on-disk artwork.
///
/// Guard rails:
/// - cannot delete the active profile (frontend must `switch_profile` first);
/// - cannot delete the last remaining profile (keeps the app usable).
///
/// After the row is removed, the profile directory is wiped from disk and the
/// `app.last_profile_id` setting is cleared if it pointed to this profile so
/// the next startup falls back to the most-recently-used remaining profile.
#[tauri::command]
pub async fn delete_profile(state: tauri::State<'_, AppState>, profile_id: i64) -> AppResult<()> {
    let active_id = {
        let guard = state.profile.read().await;
        guard.as_ref().map(|p| p.profile_id)
    };
    if active_id == Some(profile_id) {
        return Err(AppError::Other(
            "cannot delete the active profile; switch to another profile first".into(),
        ));
    }

    let repo = profile_repo(&state);
    match repo.delete_guarded(profile_id).await? {
        ProfileDeleteOutcome::Deleted => {}
        ProfileDeleteOutcome::WasLast => {
            return Err(AppError::Other(
                "cannot delete the last remaining profile".into(),
            ));
        }
        ProfileDeleteOutcome::NotFound => {
            return Err(AppError::ProfileNotFound(profile_id));
        }
    }

    // Clear the last-profile pointer if it referenced the deleted profile so
    // startup doesn't try to reopen a profile that no longer exists.
    sqlx::query(
        "DELETE FROM app_setting
          WHERE key = 'app.last_profile_id' AND value = ?",
    )
    .bind(profile_id.to_string())
    .execute(&state.app_db)
    .await?;

    let dir = state.paths.profile_dir(profile_id);
    if dir.exists() {
        // `remove_dir_all` walks the tree synchronously — on a profile
        // with a populated artwork cache that's enough I/O to noticeably
        // stall the tokio runtime. Push it off to the blocking pool so
        // the command (and every queued sibling) stays responsive.
        let dir_for_blocking = dir.clone();
        let removal =
            tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir_for_blocking)).await;
        match removal {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                // The DB row is already gone — log so the user can clean up
                // the stale directory manually, but don't fail the command.
                tracing::warn!(profile_id, path = %dir.display(), %err, "failed to remove profile directory");
            }
            Err(join_err) => {
                tracing::warn!(
                    profile_id,
                    path = %dir.display(),
                    %join_err,
                    "remove_dir_all join failed; directory may have been partially removed"
                );
            }
        }
    }

    Ok(())
}

/// Read a single key from the active profile's `profile_setting` table.
/// Returns `None` when the key is missing — the caller is responsible
/// for falling back to a default. Generic enough to back the UI's sort
/// memory and single-click toggle.
#[tauri::command]
pub async fn get_profile_setting(
    state: tauri::State<'_, AppState>,
    key: String,
) -> AppResult<Option<String>> {
    let pool = state.require_profile_pool().await?;
    let value: Option<(String,)> =
        sqlx::query_as("SELECT value FROM profile_setting WHERE key = ?")
            .bind(&key)
            .fetch_optional(&pool)
            .await?;
    Ok(value.map(|(v,)| v))
}

/// Upsert a `profile_setting` row. `value_type` is the typed marker
/// stored alongside the raw string ("bool", "int", "json", ...).
#[tauri::command]
pub async fn set_profile_setting(
    state: tauri::State<'_, AppState>,
    key: String,
    value: String,
    value_type: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    sqlx::query(
        r#"
        INSERT INTO profile_setting (key, value, value_type, updated_at)
        VALUES (?, ?, ?, strftime('%s','now')*1000)
        ON CONFLICT(key) DO UPDATE
           SET value = excluded.value,
               value_type = excluded.value_type,
               updated_at = excluded.updated_at
        "#,
    )
    .bind(&key)
    .bind(&value)
    .bind(&value_type)
    .execute(&pool)
    .await?;
    Ok(())
}
