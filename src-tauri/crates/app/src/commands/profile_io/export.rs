//! Exporting a profile into a `.waveflow` archive.

use std::path::Path;

use chrono::Utc;
use sqlx::SqlitePool;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

/// Export the active profile (or `profile_id` if provided) into a
/// `.waveflow` archive at `target_path`. Overwrites if the file
/// already exists.
#[tauri::command]
pub async fn export_profile(
    state: tauri::State<'_, AppState>,
    profile_id: Option<i64>,
    target_path: String,
) -> AppResult<()> {
    // The UI can only say "export failed"; the reason goes to the log,
    // where the message points the user.
    export_profile_inner(&state, profile_id, target_path)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "profile export failed"))
}

async fn export_profile_inner(
    state: &AppState,
    profile_id: Option<i64>,
    target_path: String,
) -> AppResult<()> {
    let profile_id = match profile_id {
        Some(id) => id,
        None => state.require_profile_id().await?,
    };

    // Look up the source profile name for the manifest. Fail loudly if
    // the row is gone — caller passed a stale id.
    let profile_name: String = sqlx::query_scalar("SELECT name FROM profile WHERE id = ?")
        .bind(profile_id)
        .fetch_optional(&state.app_db)
        .await?
        .ok_or(AppError::ProfileNotFound(profile_id))?;

    // Fold any pending WAL pages back into the main file before we copy
    // it — otherwise the archive holds a partial snapshot.
    checkpoint_profile_wal(state, profile_id).await?;

    let profile_dir = state.paths.profile_dir(profile_id);
    let db_path = state.paths.profile_db(profile_id);
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_artwork_dir = if read_include_metadata_artwork(&state.app_db).await? {
        Some(state.paths.metadata_artwork_dir.clone())
    } else {
        None
    };

    let manifest = ArchiveManifest {
        archive_version: ARCHIVE_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        profile_name,
        source_profile_id: profile_id,
        exported_at: Utc::now().to_rfc3339(),
    };

    // CPU-bound work (zip + read all artwork) → spawn_blocking so the
    // tokio runtime stays responsive while a multi-GB library is
    // packaged. The closure owns its inputs by value to side-step the
    // 'static bound spawn_blocking imposes.
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        write_archive(
            Path::new(&target_path),
            &profile_dir,
            &db_path,
            &artwork_dir,
            metadata_artwork_dir.as_deref(),
            &manifest,
        )
    })
    .await
    .map_err(|e| AppError::Other(format!("export task join: {e}")))??;

    Ok(())
}

/// Read the `backup.include_metadata_artwork` app setting. Defaults to
/// `true` so a fresh install + first manual export produces a complete
/// archive without the user having to opt in.
pub(crate) async fn read_include_metadata_artwork(app_db: &SqlitePool) -> AppResult<bool> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = 'backup.include_metadata_artwork'",
    )
    .fetch_optional(app_db)
    .await?;
    Ok(row.map(|v| v == "true" || v == "1").unwrap_or(true))
}
