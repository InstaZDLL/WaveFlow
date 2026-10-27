//! The profile-wide album list and the album search.

use std::path::PathBuf;

use serde::Serialize;
use sqlx::FromRow;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Slim album row shipped by `list_albums` — artwork is represented by
/// `(hash, format, has_1x, has_2x)` so the response-level
/// `artwork_base` carries the per-profile prefix once instead of
/// repeating it on every row.
#[derive(Debug, Clone, Serialize)]
pub struct AlbumRow {
    pub id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    /// Best-quality bit depth across the album's tracks. Drives the
    /// Hi-Res cover badge — if any track in the album is mastered at
    /// 24-bit, the badge shows on the cover. `None` when no track
    /// has a known bit depth (e.g. all MP3s).
    pub max_bit_depth: Option<i64>,
    pub max_sample_rate: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListAlbumsResponse {
    /// Per-profile artwork dir. Stitch `<base>/<hash>.<format>` for the
    /// full image, `<base>/<hash>_1x.jpg` / `<base>/<hash>_2x.jpg` for
    /// thumbnails (the thumbnail pipeline always emits JPEG regardless
    /// of the source extension).
    pub artwork_base: String,
    pub items: Vec<AlbumRow>,
}

/// Private SQL row — the public `AlbumRow` derives `artwork_path` from the
/// per-profile data dir in Rust.
#[derive(FromRow)]
struct AlbumRawRow {
    id: i64,
    title: String,
    artist_name: Option<String>,
    year: Option<i64>,
    track_count: i64,
    total_duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    max_bit_depth: Option<i64>,
    max_sample_rate: Option<i64>,
}

/// Whitelisted ORDER BY clause builder for `list_albums`. Falls back to
/// the default "Artist → Album" sort whenever the spec isn't recognized.
fn album_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(order_by, Some("year") | Some("added_at"));
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("title"), "ASC") => "ORDER BY al.canonical_title COLLATE NAME ASC",
        (Some("title"), "DESC") => "ORDER BY al.canonical_title COLLATE NAME DESC",
        (Some("artist"), "ASC") => "ORDER BY ar.canonical_name COLLATE NAME ASC, al.canonical_title COLLATE NAME",
        (Some("artist"), "DESC") => "ORDER BY ar.canonical_name COLLATE NAME DESC, al.canonical_title COLLATE NAME",
        (Some("year"), "ASC") => "ORDER BY al.year ASC, al.canonical_title COLLATE NAME",
        (Some("year"), "DESC") => "ORDER BY al.year DESC, al.canonical_title COLLATE NAME",
        (Some("added_at"), "ASC") => "ORDER BY MIN(t.added_at) ASC",
        (Some("added_at"), "DESC") => "ORDER BY MIN(t.added_at) DESC",
        _ => "ORDER BY ar.canonical_name COLLATE NAME,\n                  al.canonical_title COLLATE NAME",
    }
}

/// List every album that has at least one available track in the given
/// library, sorted by artist → album title. Track count and total duration
/// are computed on the fly so the UI can display "Album · N titres · h:mm".
#[tauri::command]
pub async fn list_albums(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListAlbumsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = album_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT al.id,
               al.title,
               COALESCE(ar.name, al.album_artist) AS artist_name,
               al.year,
               COUNT(t.id)                     AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash                         AS artwork_hash,
               aw.format                       AS artwork_format,
               MAX(t.bit_depth)                AS max_bit_depth,
               MAX(t.sample_rate)              AS max_sample_rate
          FROM album al
          JOIN track t        ON t.album_id = al.id
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
         GROUP BY al.id
         {order_clause}
"#
    );

    let raw = sqlx::query_as::<_, AlbumRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&*pool)
        .await?;

    let items = expand_album_rows(raw, artwork_dir.clone()).await?;

    Ok(ListAlbumsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch the thumbnail-existence flags onto a batch of raw album rows.
///
/// Per-row mapping does N synchronous `Path::exists` probes against the
/// artwork dir (via `thumbnail_paths_for`). At 850+ albums × 2 checks
/// that's enough sustained syscalls to noticeably stall the tokio
/// runtime, so we hand the whole batch off to the blocking pool in one
/// shot — single hop, no per-row overhead. Shared by `list_albums` and
/// `search_albums`.
async fn expand_album_rows(
    raw: Vec<AlbumRawRow>,
    artwork_dir: PathBuf,
) -> AppResult<Vec<AlbumRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|row| {
                let (artwork_has_1x, artwork_has_2x) = match row.artwork_hash.as_deref() {
                    Some(hash) => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                AlbumRow {
                    id: row.id,
                    title: row.title,
                    artist_name: row.artist_name,
                    year: row.year,
                    track_count: row.track_count,
                    total_duration_ms: row.total_duration_ms,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    max_bit_depth: row.max_bit_depth,
                    max_sample_rate: row.max_sample_rate,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("album row expand join: {e}")))
}

/// Search albums by name for the global top-bar search. Matches the
/// query's [`canonical_name`](waveflow_core::scanner::canonical_name)
/// form as a substring of `album.canonical_title` (case / accent
/// insensitive via the canonical column, no FTS — `track_fts` is
/// track-scoped). Prefix matches rank first. Returns the same slim
/// `{ artwork_base, items }` shape as [`list_albums`] so the frontend
/// reuses `expandAlbumRow`.
#[tauri::command]
pub async fn search_albums(
    state: tauri::State<'_, AppState>,
    query: String,
    library_id: Option<i64>,
    limit: Option<i64>,
) -> AppResult<ListAlbumsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let canon = waveflow_core::scanner::canonical_name(query.trim());
    if canon.is_empty() {
        return Ok(ListAlbumsResponse {
            artwork_base: artwork_dir.to_string_lossy().into_owned(),
            items: Vec::new(),
        });
    }
    // Small dropdown sections — clamp to keep the payload + the
    // per-row thumbnail probes bounded.
    let limit = limit.unwrap_or(8).clamp(1, 50);

    let raw = sqlx::query_as::<_, AlbumRawRow>(
        r#"
        SELECT al.id,
               al.title,
               COALESCE(ar.name, al.album_artist) AS artist_name,
               al.year,
               COUNT(t.id)                     AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash                         AS artwork_hash,
               aw.format                       AS artwork_format,
               MAX(t.bit_depth)                AS max_bit_depth,
               MAX(t.sample_rate)              AS max_sample_rate
          FROM album al
          JOIN track t        ON t.album_id = al.id
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
           AND instr(al.canonical_title, ?) > 0
         GROUP BY al.id
         ORDER BY (instr(al.canonical_title, ?) = 1) DESC,
                  al.canonical_title COLLATE NAME
         LIMIT ?
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .bind(&canon)
    .bind(&canon)
    .bind(limit)
    .fetch_all(&*pool)
    .await?;

    let items = expand_album_rows(raw, artwork_dir.clone()).await?;

    Ok(ListAlbumsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}
