//! Importing a `.waveflow` archive as a new profile, under a free name,
//! and cleaning up after a failed import.

use std::path::Path;

use chrono::Utc;
use sqlx::SqlitePool;

use crate::{
    db,
    error::{AppError, AppResult},
    paths::AppPaths,
    state::AppState,
};

use super::*;

/// Import a `.waveflow` archive as a brand-new profile. The new
/// profile is **not** activated automatically — the caller can switch
/// to it via `switch_profile` once the user picks it from the
/// selector. Returns the new profile's id and name.
#[tauri::command]
pub async fn import_profile(
    state: tauri::State<'_, AppState>,
    source_path: String,
    name: Option<String>,
) -> AppResult<ImportedProfile> {
    // Same as the export: the UI has one generic sentence, the log has
    // the cause. #767 took a user report to find a UNIQUE violation.
    import_profile_inner(&state, source_path, name)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "profile import failed"))
}

async fn import_profile_inner(
    state: &AppState,
    source_path: String,
    name: Option<String>,
) -> AppResult<ImportedProfile> {
    // 1. Inspect the archive on a blocking thread (file I/O + zip
    //    decompression). Manifest parsing happens here so we fail fast
    //    on a truly broken file before touching the DB.
    let manifest = tokio::task::spawn_blocking({
        let source_path = source_path.clone();
        move || read_manifest(Path::new(&source_path))
    })
    .await
    .map_err(|e| AppError::Other(format!("import inspect join: {e}")))??;

    if manifest.archive_version != ARCHIVE_VERSION {
        return Err(AppError::Other(format!(
            "incompatible archive (version {}, expected {})",
            manifest.archive_version, ARCHIVE_VERSION
        )));
    }

    // 2. Allocate a fresh profile row. We reuse the standard creation
    //    path so the row + filesystem layout match what `create_profile`
    //    would produce — minus the empty data.db, which we overwrite
    //    from the archive in step 3.
    let profile_name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or(manifest.profile_name.clone());

    let (new_profile_id, profile_name) =
        insert_with_free_name(&state.app_db, &profile_name).await?;
    let rel_dir = AppPaths::profile_rel_dir(new_profile_id);
    sqlx::query("UPDATE profile SET data_dir = ? WHERE id = ?")
        .bind(&rel_dir)
        .bind(new_profile_id)
        .execute(&state.app_db)
        .await?;

    state.paths.ensure_profile_dirs(new_profile_id)?;

    let new_profile_dir = state.paths.profile_dir(new_profile_id);
    let new_db_path = state.paths.profile_db(new_profile_id);
    let new_artwork_dir = state.paths.profile_artwork_dir(new_profile_id);
    let metadata_artwork_dir = state.paths.metadata_artwork_dir.clone();

    // 3. Extract — also blocking. On any failure, roll the profile row
    //    back so the user doesn't end up with a stub profile that
    //    points at a half-written directory.
    let extract_result = tokio::task::spawn_blocking(move || {
        extract_archive(
            Path::new(&source_path),
            &new_profile_dir,
            &new_db_path,
            &new_artwork_dir,
            &metadata_artwork_dir,
        )
    })
    .await
    .map_err(|e| AppError::Other(format!("import extract join: {e}")))?;

    if let Err(err) = extract_result {
        cleanup_partial_profile(state, new_profile_id).await;
        return Err(err);
    }

    // 4. Normalise the bundled `_sqlx_migrations.checksum` column against
    //    the local migration files before running the migrator. Archives
    //    produced by a build whose migration files happened to be
    //    checked out with CRLF endings (Git `core.autocrlf=true` on
    //    Windows + no `.gitattributes` lock) store SHA-384 hashes
    //    computed on different bytes than the current LF-normalised
    //    sources, even though the SQL is semantically identical. Without
    //    this step sqlx refuses the import with
    //    "migration X was previously applied but has been modified".
    //    See `.gitattributes` for the forward fix.
    if let Err(err) = normalise_migration_checksums(&state.paths.profile_db(new_profile_id)).await {
        cleanup_partial_profile(state, new_profile_id).await;
        return Err(err);
    }

    // 5. Open + close the imported pool once so any pending migrations
    //    (the source might be older than the local schema) replay
    //    immediately. This matches the create_profile flow and gives
    //    the user a usable profile by the time the call returns.
    let pool =
        match db::profile_db::open(&state.paths.profile_db(new_profile_id), &state.paths.app_db)
            .await
        {
            Ok(pool) => pool,
            Err(err) => {
                cleanup_partial_profile(state, new_profile_id).await;
                return Err(err);
            }
        };
    pool.close().await;

    Ok(ImportedProfile {
        id: new_profile_id,
        name: profile_name,
    })
}

/// The most "(n)" suffixes an import tries before giving up. A person
/// with a thousand profiles of one name has a problem this won't solve.
const MAX_NAME_SUFFIX: u32 = 1000;

/// Insert the imported profile's row under `base`, or under `base (2)`,
/// `base (3)`… when that name is taken (#767).
///
/// `profile.name` is UNIQUE, and restoring a backup of a profile that
/// still exists is the ordinary case, not the odd one: the archive's name
/// is taken by construction. The user never chose that name, so the app
/// finds a free one rather than refusing. Retrying on the violation
/// itself, instead of checking first, leaves no window for a profile
/// created meanwhile to take the name between the check and the insert.
async fn insert_with_free_name(app_db: &SqlitePool, base: &str) -> AppResult<(i64, String)> {
    let now = Utc::now().timestamp_millis();
    for n in 1..=MAX_NAME_SUFFIX {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base} ({n})")
        };
        let inserted = sqlx::query(
            "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
             VALUES (?, 'emerald', NULL, '', ?, ?)",
        )
        .bind(&candidate)
        .bind(now)
        .bind(now)
        .execute(app_db)
        .await;
        match inserted {
            Ok(done) => return Ok((done.last_insert_rowid(), candidate)),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(AppError::Other(format!(
        "no free profile name left for \"{base}\" after {MAX_NAME_SUFFIX} tries"
    )))
}

/// Roll back a half-imported profile: remove the on-disk directory and
/// the `profile` row. Best-effort — failures are swallowed so the caller
/// can surface the *original* import error rather than a cleanup error
/// masking it. User can wipe `<app_data>/profiles/<id>/` by hand if the
/// fs delete failed (rare; usually a held file handle from a panicked
/// pool).
async fn cleanup_partial_profile(state: &AppState, profile_id: i64) {
    let _ = std::fs::remove_dir_all(state.paths.profile_dir(profile_id));
    let _ = sqlx::query("DELETE FROM profile WHERE id = ?")
        .bind(profile_id)
        .execute(&state.app_db)
        .await;
}
