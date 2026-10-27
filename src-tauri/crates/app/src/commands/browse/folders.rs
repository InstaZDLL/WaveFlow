//! The folder tab: the folder tree, a folder's tracks and its track ids.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct FolderRow {
    pub id: i64,
    pub path: String,
    pub last_scanned_at: Option<i64>,
    pub is_watched: i64,
    pub track_count: i64,
}

/// List every folder registered under the given library, along with the
/// number of tracks found inside it at the last scan.
#[tauri::command]
pub async fn list_folders(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
) -> AppResult<Vec<FolderRow>> {
    let pool = state.require_profile_pool().await?;

    let rows = sqlx::query_as::<_, FolderRow>(
        r#"
        SELECT lf.id,
               lf.path,
               lf.last_scanned_at,
               lf.is_watched,
               COALESCE(COUNT(t.id), 0) AS track_count
          FROM library_folder lf
          LEFT JOIN track t
            ON t.folder_id = lf.id AND t.is_available = 1
         WHERE (? IS NULL OR lf.library_id = ?)
         GROUP BY lf.id
         ORDER BY lf.path COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .fetch_all(&*pool)
    .await?;

    Ok(rows)
}

// ── Folder browsing (#578) ──────────────────────────────────────────

/// One directory inside a library root, as the folder browser shows it.
#[derive(Debug, Clone, Serialize)]
pub struct FolderNode {
    /// Absolute path, so it can be handed straight back as the next
    /// listing's argument.
    pub path: String,
    /// Last segment — what the row displays.
    pub name: String,
    /// **Indexed, available tracks** anywhere beneath it, not files on
    /// disk. The distinction is deliberate: the tree is derived from
    /// what the scanner indexed, so a directory holding only skipped
    /// files never appears at all, and no row can claim `0`.
    pub track_count: i64,
    /// Bytes of those same tracks.
    pub total_size: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
}

/// A directory listing: where we are, how to get back, and what is
/// directly inside.
#[derive(Debug, Clone, Serialize)]
pub struct FolderListing {
    /// Artwork directory, as everywhere else — the wrapper builds the
    /// thumbnail paths from it rather than carrying a ~70-char prefix
    /// on every row.
    pub artwork_base: String,
    /// The directory that was listed, echoed back so a late response
    /// can be matched against the row the user is now on.
    pub path: String,
    /// One level up, `None` when `path` **is** the library root: the
    /// browser stops there rather than walking out of the library.
    pub parent: Option<String>,
    /// The library root that contains `path`, for the breadcrumb.
    pub root_id: Option<i64>,
    pub root_path: Option<String>,
    pub folders: Vec<FolderNode>,
}

#[derive(FromRow)]
struct FolderNodeRaw {
    name: String,
    track_count: i64,
    total_size: i64,
    /// Only selected so the bare-column rule below has its anchor.
    #[allow(dead_code)]
    first_rest: String,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

/// The `[low, high)` band of file paths that lie under `dir`.
///
/// A range, not `LIKE dir || '%'`, for two reasons that both matter:
/// `UNIQUE (library_id, file_path)` is a BINARY b-tree, so `>=` / `<`
/// walk it; and a LIKE pattern would need `%` and `_` escaped out of a
/// path the *user* named, where an `ESCAPE` clause then costs the index
/// anyway.
///
/// `low` always ends with the separator, so `/m/Rock` cannot match
/// `/m/Rockabilly`, and `high` is `low` with that last byte incremented
/// — the separator is ASCII on every platform we build for, so the
/// increment cannot split a character.
pub(crate) fn folder_prefix_range(dir: &str) -> (String, String) {
    let mut low = dir.to_string();
    if !low.ends_with(std::path::MAIN_SEPARATOR) {
        low.push(std::path::MAIN_SEPARATOR);
    }
    let (head, sep) = low.split_at(low.len() - 1);
    let mut high = head.to_string();
    high.push(char::from(sep.as_bytes()[0] + 1));
    (low, high)
}

/// The parent of `dir`, or `None` once it reaches `root`.
///
/// String surgery rather than [`std::path::Path::parent`] because the
/// answer has to stay inside the library: a browser that can walk above
/// the root would list directories the scanner never indexed, i.e.
/// nothing at all.
pub(super) fn folder_parent(dir: &str, root: Option<&str>) -> Option<String> {
    let trimmed = dir.trim_end_matches(std::path::MAIN_SEPARATOR);
    let root = root.map(|r| r.trim_end_matches(std::path::MAIN_SEPARATOR));
    if root.is_some_and(|r| r.eq_ignore_ascii_case(trimmed)) {
        return None;
    }
    let cut = trimmed.rfind(std::path::MAIN_SEPARATOR)?;
    let parent = &trimmed[..cut];
    // Never above the root, even if one of its own segments repeats.
    match root {
        Some(r) if parent.len() < r.len() => None,
        _ if parent.is_empty() => None,
        _ => Some(parent.to_string()),
    }
}

/// SQL for the directories directly inside a path.
///
/// The grouping is done in SQLite rather than in Rust on purpose: a
/// library root can hold fifty thousand tracks, and the browser only
/// ever shows the handful of directories one level down.
///
/// `artwork_hash` / `artwork_format` are **bare columns** beside
/// `MIN(first_rest)`, which is a documented SQLite guarantee, not an
/// accident: with exactly one `min()` in the select list, bare columns
/// take their values from the row that produced that minimum. So the
/// cover shown for a directory is the cover of the first file under it
/// by path — deterministic, and the one a user would call the folder's
/// own. `folder_cover_comes_from_the_first_file` pins it.
pub(super) const FOLDER_CHILDREN_SQL: &str = r#"
    WITH sub AS (
      SELECT substr(t.file_path, ? + 1) AS rest,
             t.file_size                AS file_size,
             aw.hash                    AS artwork_hash,
             aw.format                  AS artwork_format
        FROM track t
        LEFT JOIN album   al ON al.id = t.album_id
        LEFT JOIN artwork aw ON aw.id = al.artwork_id
       WHERE (? IS NULL OR t.library_id = ?)
         AND t.is_available = 1
         AND t.file_path >= ?
         AND t.file_path <  ?
    ),
    child AS (
      SELECT substr(rest, 1, instr(rest, ?) - 1) AS name,
             rest, file_size, artwork_hash, artwork_format
        FROM sub
       WHERE instr(rest, ?) > 0
    )
    SELECT name,
           COUNT(*)                    AS track_count,
           COALESCE(SUM(file_size), 0) AS total_size,
           MIN(rest)                   AS first_rest,
           artwork_hash,
           artwork_format
      FROM child
     GROUP BY name COLLATE NOCASE
     ORDER BY name COLLATE FOLD
"#;

/// SQL for the library root that contains a path.
///
/// Longest match wins: a user may have added both a parent and a child
/// directory as roots, and the breadcrumb has to stop at the nearer one.
///
/// `rtrim(path, sep)` because a stored root may already end with the
/// separator -- a whole volume is `E:\` on Windows, and a folder picker
/// can hand back a trailing slash anywhere. Appending a second one would
/// make the comparison miss and leave the browser with no root at all:
/// no breadcrumb anchor, and nothing to stop the walk up.
pub(super) const FOLDER_ROOT_SQL: &str = r#"
    SELECT id, path
      FROM library_folder
     WHERE (? IS NULL OR library_id = ?)
       AND instr(? || ?, rtrim(path, ?) || ?) = 1
     ORDER BY length(path) DESC
     LIMIT 1
"#;

/// List the directories directly inside `path`.
///
/// The tree is derived at query time from `track.file_path`; nothing is
/// materialised. That is a measured call rather than a guess: the band
/// scan walks the `(library_id, file_path)` index and only the segment
/// one level down is grouped, so the cost is the directory's own
/// subtree, not the library.
#[tauri::command]
pub async fn browse_folders(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    path: String,
) -> AppResult<FolderListing> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // See [`FOLDER_ROOT_SQL`] for why the stored path is trimmed.
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let root: Option<(i64, String)> = sqlx::query_as(FOLDER_ROOT_SQL)
        .bind(library_id)
        .bind(library_id)
        .bind(&path)
        .bind(&sep)
        .bind(&sep)
        .bind(&sep)
        .fetch_optional(&*pool)
        .await?;

    let (low, high) = folder_prefix_range(&path);
    // Characters, not bytes: SQLite's `substr` and `instr` count
    // characters, so a path with an accent would be cut mid-name by a
    // byte length.
    let prefix_chars = low.chars().count() as i64;

    let raw = sqlx::query_as::<_, FolderNodeRaw>(FOLDER_CHILDREN_SQL)
        .bind(prefix_chars)
        .bind(library_id)
        .bind(library_id)
        .bind(&low)
        .bind(&high)
        .bind(&sep)
        .bind(&sep)
        .fetch_all(&*pool)
        .await?;

    let folders = {
        let artwork_dir = artwork_dir.clone();
        let low = low.clone();
        tokio::task::spawn_blocking(move || {
            raw.into_iter()
                .map(|row| {
                    let (has_1x, has_2x) = match row.artwork_hash.as_deref() {
                        Some(hash) => {
                            let (p1, p2) =
                                crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                            (p1.is_some(), p2.is_some())
                        }
                        None => (false, false),
                    };
                    FolderNode {
                        path: format!("{low}{}", row.name),
                        name: row.name,
                        track_count: row.track_count,
                        total_size: row.total_size,
                        artwork_hash: row.artwork_hash,
                        artwork_format: row.artwork_format,
                        artwork_has_1x: has_1x,
                        artwork_has_2x: has_2x,
                    }
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| AppError::Other(format!("folder listing join: {e}")))?
    };

    Ok(FolderListing {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        parent: folder_parent(&path, root.as_ref().map(|(_, p)| p.as_str())),
        root_id: root.as_ref().map(|(id, _)| *id),
        root_path: root.map(|(_, p)| p),
        path,
        folders,
    })
}

/// The tracks of one directory, in the same shape the Tracks tab uses.
///
/// Deliberately the same row type and the same ordering helper as
/// [`list_library_tracks`]: the folder view renders the existing track
/// table, so it inherits its columns, its sort, its context menu and its
/// properties modal instead of growing a second table that would drift
/// from them.
///
/// `recursive` decides what a folder *is* for this listing: `false` —
/// the browser's own use — lists the files directly inside, so the
/// directories shown beside them are not double-counted. `true` is for
/// acting on a folder as a whole.
#[tauri::command]
pub async fn list_folder_tracks(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    path: String,
    recursive: Option<bool>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListLibraryTracksResponse> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = library_track_order_clause(order_by.as_deref(), direction.as_deref());
    // A server track has no file path, so the band filter drops the
    // remote half of the union on its own — but say it, because a
    // reader should not have to derive it from a NULL comparison.
    let sql = library_tracks_sql_where(
        "AND source = 'local'
            AND file_path >= ?
            AND file_path <  ?
            AND (? = 1 OR instr(substr(file_path, ?), ?) = 0)",
        &order_clause,
    );

    let (low, high) = folder_prefix_range(&path);
    let sep = std::path::MAIN_SEPARATOR.to_string();

    let raw = sqlx::query_as::<_, LibraryTrackRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(&low)
        .bind(&high)
        .bind(i64::from(recursive.unwrap_or(false)))
        .bind(low.chars().count() as i64 + 1)
        .bind(&sep)
        .fetch_all(&*pool)
        .await?;

    let items = expand_library_track_rows(raw, artwork_dir.clone()).await?;

    Ok(ListLibraryTracksResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Every track under `path`, recursively, in path order.
///
/// What "play this folder", "add this folder to the queue" and the
/// batch tag editor all need: ids, in the order the files sit on disk,
/// which is the order a folder-organised listener expects. Kept apart
/// from [`list_folder_tracks`] because none of those actions wants the
/// forty columns a table row carries.
#[tauri::command]
pub async fn folder_track_ids(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    path: String,
) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    let (low, high) = folder_prefix_range(&path);

    let ids = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT id
          FROM track
         WHERE (? IS NULL OR library_id = ?)
           AND is_available = 1
           AND file_path >= ?
           AND file_path <  ?
         ORDER BY file_path COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .bind(&low)
    .bind(&high)
    .fetch_all(&*pool)
    .await?;

    Ok(ids)
}
