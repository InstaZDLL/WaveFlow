use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{
    commands::scan::{scan_folder_inner, ScanSummary},
    error::{AppError, AppResult},
    state::AppState,
    watcher::{apply_toggle, WatcherManager},
};

/// Library row returned to the frontend, with denormalized counts computed on
/// the fly so the sidebar can display "X titres · Y albums" without issuing a
/// second query per library.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub color_id: String,
    pub icon_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub track_count: i64,
    pub album_count: i64,
    pub artist_count: i64,
    pub genre_count: i64,
    pub folder_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateLibraryInput {
    pub name: String,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

/// Partial update payload — any field left as `None` is preserved via
/// SQL `COALESCE`. The description cannot be cleared through this shape,
/// which is fine for the current UX.
#[derive(Debug, Deserialize)]
pub struct UpdateLibraryInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

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

/// List every library in the active profile's database, most-recently-updated
/// first, with track / album / folder counts.
#[tauri::command]
pub async fn list_libraries(state: tauri::State<'_, AppState>) -> AppResult<Vec<Library>> {
    let pool = state.require_profile_pool().await?;

    // A single query with LEFT JOIN + COUNT(DISTINCT ...) keeps ordering stable
    // and avoids the N+1 problem when rendering the sidebar.
    let libraries = sqlx::query_as::<_, Library>(
        r#"
        SELECT l.id, l.name, l.description, l.color_id, l.icon_id,
               l.created_at, l.updated_at,
               COALESCE(tc.track_count,  0) AS track_count,
               COALESCE(tc.album_count,  0) AS album_count,
               COALESCE(tc.artist_count, 0) AS artist_count,
               COALESCE(gc.genre_count,  0) AS genre_count,
               COALESCE(f.folder_count,  0) AS folder_count
          FROM library l
          LEFT JOIN (
              SELECT library_id,
                     COUNT(*)                         AS track_count,
                     COUNT(DISTINCT album_id)         AS album_count,
                     COUNT(DISTINCT primary_artist)   AS artist_count
                FROM track
               WHERE is_available = 1
               GROUP BY library_id
          ) tc ON tc.library_id = l.id
          LEFT JOIN (
              SELECT t.library_id, COUNT(DISTINCT tg.genre_id) AS genre_count
                FROM track t
                JOIN track_genre tg ON tg.track_id = t.id
               WHERE t.is_available = 1
               GROUP BY t.library_id
          ) gc ON gc.library_id = l.id
          LEFT JOIN (
              SELECT library_id, COUNT(*) AS folder_count
                FROM library_folder
               GROUP BY library_id
          ) f ON f.library_id = l.id
         ORDER BY l.updated_at DESC
        "#,
    )
    .fetch_all(&pool)
    .await?;

    Ok(libraries)
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

    let pool = state.require_profile_pool().await?;

    let insert = sqlx::query(
        "INSERT INTO library (name, description, color_id, icon_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&name)
    .bind(input.description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await?;

    let id = insert.last_insert_rowid();

    Ok(Library {
        id,
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
    let pool = state.require_profile_pool().await?;

    // Validate the library exists up-front so the caller gets a precise
    // error instead of a "0 rows updated" silent no-op.
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM library WHERE id = ?")
        .bind(library_id)
        .fetch_optional(&pool)
        .await?;
    if exists.is_none() {
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

    let now = now_millis();
    sqlx::query(
        "UPDATE library
            SET name        = COALESCE(?, name),
                description = COALESCE(?, description),
                color_id    = COALESCE(?, color_id),
                icon_id     = COALESCE(?, icon_id),
                updated_at  = ?
          WHERE id = ?",
    )
    .bind(trimmed_name.as_deref())
    .bind(input.description.as_deref())
    .bind(input.color_id.as_deref())
    .bind(input.icon_id.as_deref())
    .bind(now)
    .bind(library_id)
    .execute(&pool)
    .await?;

    Ok(())
}

/// Delete a library. The `ON DELETE CASCADE` on `library_folder` and
/// `track` walks the transitive graph (tracks → track_artist / track_genre
/// / lyrics / track_analysis / playlist_track / liked_track / play_event /
/// queue_item / scrobble_queue) so the caller only has to issue the one
/// DELETE and the DB takes care of the rest.
#[tauri::command]
pub async fn delete_library(
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;

    let result = sqlx::query("DELETE FROM library WHERE id = ?")
        .bind(library_id)
        .execute(&pool)
        .await?;

    if result.rows_affected() == 0 {
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

    let folder_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM library_folder WHERE library_id = ? ORDER BY id",
    )
    .bind(library_id)
    .fetch_all(&pool)
    .await?;

    let mut total = RescanSummary {
        library_id,
        ..Default::default()
    };

    for folder_id in folder_ids {
        match scan_folder_inner(&pool, &artwork_dir, folder_id).await {
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
    sqlx::query("UPDATE library SET updated_at = ? WHERE id = ?")
        .bind(now_millis())
        .bind(library_id)
        .execute(&pool)
        .await?;

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
    let pool = state.require_profile_pool().await?;

    // Validate the library exists to return a precise error rather than a
    // foreign-key constraint failure.
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM library WHERE id = ?")
        .bind(library_id)
        .fetch_optional(&pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::Other(format!(
            "library {library_id} does not exist in active profile"
        )));
    }

    let result = sqlx::query(
        "INSERT INTO library_folder (library_id, path, last_scanned_at, is_watched)
         VALUES (?, ?, NULL, 0)",
    )
    .bind(library_id)
    .bind(&path)
    .execute(&pool)
    .await?;

    Ok(result.last_insert_rowid())
}

/// Row shape for the per-library folder list — only the bits the UI
/// needs (path, scan timestamp, watch flag). Counts come from
/// `list_folders` in `browse.rs`; this command is dedicated to the
/// folder management surface (toggle watcher, see scan timestamps).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct LibraryFolder {
    pub id: i64,
    pub library_id: i64,
    pub path: String,
    pub last_scanned_at: Option<i64>,
    pub is_watched: i64,
}

/// List every folder for a library, with its watch flag. Returned
/// straight from `library_folder` so toggling reflects on next fetch
/// without going through the heavier `list_folders` aggregation.
#[tauri::command]
pub async fn list_library_folders(
    state: tauri::State<'_, AppState>,
    library_id: i64,
) -> AppResult<Vec<LibraryFolder>> {
    let pool = state.require_profile_pool().await?;
    let rows = sqlx::query_as::<_, LibraryFolder>(
        "SELECT id, library_id, path, last_scanned_at, is_watched
           FROM library_folder
          WHERE library_id = ?
          ORDER BY id",
    )
    .bind(library_id)
    .fetch_all(&pool)
    .await?;
    Ok(rows)
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
