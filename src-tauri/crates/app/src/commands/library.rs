use chrono::Utc;
use serde::Serialize;

use waveflow_core::repository::{
    library::{LibraryDraft, LibraryRepository, LibraryUpdate},
    sqlite::SqliteLibraryRepository,
};

use crate::{
    commands::scan::{scan_folder_inner, ScanSummary},
    error::{AppError, AppResult},
    state::AppState,
    watcher::{apply_toggle, WatcherManager},
};
// `Library` + input DTOs moved to `waveflow_core::domain::library` in the
// Phase 1.a refactor. Re-exported so existing call sites keep resolving.
// `LibraryFolder` moved alongside them in step 5.b.
pub use waveflow_core::domain::library::{
    CreateLibraryInput, Library, LibraryFolder, UpdateLibraryInput,
};

/// Aggregate result returned by `rescan_library` — summed across every
/// registered folder. `folders` is the number of folders walked so the UI
/// can tell the user "3 dossiers, 129 titres rafraîchis".
#[derive(Debug, Serialize, Default)]
pub struct RescanSummary {
    pub library_id: i64,
    pub folders: u32,
    pub scanned: u32,
    pub added: u32,
    pub updated: u32,
    pub skipped: u32,
    pub errors: u32,
    pub removed: u32,
}

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

async fn library_repo(state: &AppState) -> AppResult<SqliteLibraryRepository> {
    Ok(SqliteLibraryRepository::new(
        state.require_profile_pool().await?,
    ))
}

/// List every library in the active profile's database, most-recently-updated
/// first, with track / album / folder counts.
#[tauri::command]
pub async fn list_libraries(state: tauri::State<'_, AppState>) -> AppResult<Vec<Library>> {
    Ok(library_repo(&state).await?.list_all_with_counts().await?)
}

/// Create a new library in the active profile. The UI is expected to follow
/// this call with [`add_folder_to_library`] + scan to actually populate it.
#[tauri::command]
pub async fn create_library(
    state: tauri::State<'_, AppState>,
    input: CreateLibraryInput,
) -> AppResult<Library> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("library name cannot be empty".into()));
    }
    let color_id = input.color_id.unwrap_or_else(|| "emerald".to_string());
    let icon_id = input.icon_id.unwrap_or_else(|| "library".to_string());
    let now = now_millis();

    let draft = LibraryDraft {
        name: name.clone(),
        description: input.description.clone(),
        color_id: color_id.clone(),
        icon_id: icon_id.clone(),
        now_ms: now,
    };
    let id = library_repo(&state).await?.insert(&draft).await?;

    Ok(Library {
        id,
        // Single-tenant: the desktop's `library` table has no
        // `profile_id` column. The `0` sentinel matches what
        // `#[sqlx(default)]` would yield from a SELECT that omits
        // the column anyway, so writing it explicitly keeps the
        // round-trip consistent.
        profile_id: 0,
        name,
        description: input.description,
        color_id,
        icon_id,
        created_at: now,
        updated_at: now,
        track_count: 0,
        album_count: 0,
        artist_count: 0,
        genre_count: 0,
        folder_count: 0,
    })
}

/// Partial update of an existing library. Only the fields present in
/// `input` are written — the others are preserved. Bumps `updated_at` so
/// any listener keyed on that column (e.g. the track/albums views) will
/// auto-refresh.
#[tauri::command]
pub async fn update_library(
    state: tauri::State<'_, AppState>,
    library_id: i64,
    input: UpdateLibraryInput,
) -> AppResult<()> {
    let repo = library_repo(&state).await?;

    // Validate the library exists up-front so the caller gets a precise
    // error instead of a "0 rows updated" silent no-op.
    if !repo.exists(library_id).await? {
        return Err(AppError::Other(format!(
            "library {library_id} not found in active profile"
        )));
    }

    let trimmed_name = input.name.as_ref().map(|s| s.trim().to_string());
    if let Some(name) = &trimmed_name {
        if name.is_empty() {
            return Err(AppError::Other("library name cannot be empty".into()));
        }
    }

    let patch = LibraryUpdate {
        name: trimmed_name,
        description: input.description,
        color_id: input.color_id,
        icon_id: input.icon_id,
    };
    repo.update(library_id, &patch, now_millis()).await?;
    Ok(())
}

/// Delete a library. The `ON DELETE CASCADE` on `library_folder` and
/// `track` walks the transitive graph (tracks → track_artist / track_genre
/// / lyrics / track_analysis / playlist_track / liked_track / play_event /
/// queue_item / scrobble_queue) so the caller only has to issue the one
/// DELETE and the DB takes care of the rest.
#[tauri::command]
pub async fn delete_library(state: tauri::State<'_, AppState>, library_id: i64) -> AppResult<()> {
    if !library_repo(&state).await?.delete(library_id).await? {
        return Err(AppError::Other(format!(
            "library {library_id} not found in active profile"
        )));
    }
    tracing::info!(library_id, "library deleted");
    Ok(())
}

/// Re-scan every folder registered under a library. Folders are processed
/// sequentially — the per-file `(modified, hash)` skip inside
/// [`scan_folder_inner`] keeps re-scans cheap when nothing has changed.
#[tauri::command]
pub async fn rescan_library(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<RescanSummary> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let repo = SqliteLibraryRepository::new(pool.clone());

    let folder_ids = repo.list_folder_ids(library_id).await?;

    let mut total = RescanSummary {
        library_id,
        ..Default::default()
    };

    for folder_id in folder_ids {
        match scan_folder_inner(&pool, &artwork_dir, folder_id, Some(&app)).await {
            Ok(summary) => {
                total.folders += 1;
                let ScanSummary {
                    scanned,
                    added,
                    updated,
                    skipped,
                    errors,
                    removed,
                    ..
                } = summary;
                total.scanned += scanned;
                total.added += added;
                total.updated += updated;
                total.skipped += skipped;
                total.errors += errors;
                total.removed += removed;
            }
            Err(err) => {
                tracing::warn!(folder_id, error = %err, "rescan folder failed");
                total.errors += 1;
            }
        }
    }

    // Bump library.updated_at so the UI (keyed on this field) re-renders
    // the track/album lists, even when individual folder scans noop'd.
    repo.touch_updated_at(library_id, now_millis()).await?;

    if total.added > 0 {
        crate::commands::analysis::maybe_auto_analyze(&app);
    }

    Ok(total)
}

/// Register an absolute filesystem path as a folder of an existing library.
///
/// Returns the `library_folder.id` so the caller can immediately trigger a
/// scan on it. A `(library_id, path)` collision is surfaced as an error so
/// the UI can prompt the user to re-scan the existing folder instead.
#[tauri::command]
pub async fn add_folder_to_library(
    state: tauri::State<'_, AppState>,
    library_id: i64,
    path: String,
) -> AppResult<i64> {
    if path.trim().is_empty() {
        return Err(AppError::Other("folder path cannot be empty".into()));
    }
    let repo = library_repo(&state).await?;

    // Validate the library exists to return a precise error rather than a
    // foreign-key constraint failure.
    if !repo.exists(library_id).await? {
        return Err(AppError::Other(format!(
            "library {library_id} does not exist in active profile"
        )));
    }

    Ok(repo.insert_folder(library_id, &path).await?)
}

/// List every folder for a library, with its watch flag. Returned
/// straight from `library_folder` so toggling reflects on next fetch
/// without going through the heavier `list_folders` aggregation.
#[tauri::command]
pub async fn list_library_folders(
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<Vec<LibraryFolder>> {
    Ok(library_repo(&state).await?.list_folders(library_id).await?)
}

/// Import a list of arbitrary filesystem paths into a library.
/// Used by the drag-and-drop handler — the user can drop a mix of
/// folders and audio files; we resolve each into a folder path
/// (file → its parent dir), dedupe, then add each as a
/// `library_folder` (skipping duplicates via UNIQUE) and scan it.
///
/// Aggregates every scan's stats into a single `ScanSummary` so the
/// UI can show one toast with the total counts.
#[tauri::command]
pub async fn import_paths(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    library_id: i64,
    paths: Vec<String>,
) -> AppResult<ScanSummary> {
    if paths.is_empty() {
        return Ok(ScanSummary::default());
    }
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let repo = SqliteLibraryRepository::new(pool.clone());

    if !repo.exists(library_id).await? {
        return Err(AppError::Other(format!(
            "library {library_id} does not exist in active profile"
        )));
    }

    // Resolve each input path into the folder we should add. Files
    // contribute their parent directory; non-existent paths are
    // skipped silently (the user may have dropped a stale shortcut).
    let mut folder_paths: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in paths {
        let p = std::path::PathBuf::from(&raw);
        let folder = match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() => p,
            Ok(_) => match p.parent() {
                Some(parent) => parent.to_path_buf(),
                None => continue,
            },
            Err(err) => {
                tracing::warn!(path = %raw, %err, "import_paths: stat failed");
                continue;
            }
        };
        let canonical = folder.to_string_lossy().to_string();
        if seen.insert(canonical.clone()) {
            folder_paths.push(canonical);
        }
    }

    let mut total = ScanSummary::default();
    for path in folder_paths {
        let folder_id = repo.insert_or_get_folder(library_id, &path).await?;

        match crate::commands::scan::scan_folder_inner(&pool, &artwork_dir, folder_id, Some(&app))
            .await
        {
            Ok(summary) => {
                total.scanned += summary.scanned;
                total.added += summary.added;
                total.updated += summary.updated;
                total.skipped += summary.skipped;
                total.errors += summary.errors;
                total.removed += summary.removed;
            }
            Err(err) => {
                tracing::warn!(folder_id, path = %path, %err, "import_paths: scan failed");
                total.errors += 1;
            }
        }
    }

    repo.touch_updated_at(library_id, now_millis()).await?;

    if total.added > 0 {
        crate::commands::analysis::maybe_auto_analyze(&app);
    }
    Ok(total)
}

/// Remove a folder from a library. Detaches the in-memory watcher,
/// deletes every track that lives under this folder (so the library
/// counts and FTS index stay consistent), then drops the folder row
/// itself. The schema's `track.folder_id ON DELETE SET NULL` would
/// otherwise leave orphan tracks with `library_id` still set —
/// matching the disk would then require a full rescan.
///
/// Emits `library:rescanned` so every consumer view (LibraryContext,
/// FolderList, sidebar counts) refreshes without new wiring.
#[tauri::command]
pub async fn remove_folder_from_library(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    watcher: tauri::State<'_, std::sync::Arc<WatcherManager>>,
    folder_id: i64,
) -> AppResult<()> {
    use tauri::Emitter;

    // Detach the watcher first so a midway notify event doesn't try
    // to write back into a row we're about to delete.
    watcher.unwatch(folder_id);

    library_repo(&state)
        .await?
        .delete_folder_with_tracks(folder_id)
        .await?;

    let _ = app.emit("library:rescanned", ());
    Ok(())
}

/// Toggle whether a folder is watched for filesystem changes. Updates
/// `library_folder.is_watched` and arms or disarms the in-memory
/// watcher so the change takes effect without restarting the app.
#[tauri::command]
pub async fn set_folder_watched(
    state: tauri::State<'_, AppState>,
    watcher: tauri::State<'_, std::sync::Arc<WatcherManager>>,
    folder_id: i64,
    enable: bool,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    apply_toggle(&watcher, &pool, folder_id, enable).await
}
