//! The profile-wide artist list and the artist search.

use std::path::PathBuf;

use serde::Serialize;
use sqlx::FromRow;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Slim artist row — same wire-format contract as `AlbumRow`. Two
/// hash families (local `artwork_*` and Deezer-cached `picture_*`)
/// because the UI prefers the extracted local image and only falls
/// back to the Deezer cache when the local one is missing.
#[derive(Debug, Clone, Serialize)]
pub struct ArtistRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub album_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    /// Cached-Deezer picture hash. Files are stored under the shared
    /// `metadata_artwork_base`, always as `<hash>.jpg`.
    pub picture_hash: Option<String>,
    pub picture_has_1x: bool,
    pub picture_has_2x: bool,
    /// Deezer CDN URL — last-resort fallback when no local file is
    /// available (e.g. when the cache was wiped or the picture is on a
    /// remote profile being browsed offline).
    pub picture_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListArtistsResponse {
    pub artwork_base: String,
    pub metadata_artwork_base: String,
    pub items: Vec<ArtistRow>,
}

#[derive(FromRow)]
struct ArtistRowRaw {
    id: i64,
    name: String,
    track_count: i64,
    album_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    picture_url: Option<String>,
    picture_hash: Option<String>,
}

/// Whitelisted ORDER BY clause builder for `list_artists`.
fn artist_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(order_by, Some("albums_count") | Some("tracks_count"));
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
        (Some("name"), "ASC") => "ORDER BY ar.canonical_name COLLATE NAME ASC",
        (Some("name"), "DESC") => "ORDER BY ar.canonical_name COLLATE NAME DESC",
        (Some("albums_count"), "ASC") => "ORDER BY album_count ASC, ar.canonical_name COLLATE NAME",
        (Some("albums_count"), "DESC") => {
            "ORDER BY album_count DESC, ar.canonical_name COLLATE NAME"
        }
        (Some("tracks_count"), "ASC") => "ORDER BY track_count ASC, ar.canonical_name COLLATE NAME",
        (Some("tracks_count"), "DESC") => {
            "ORDER BY track_count DESC, ar.canonical_name COLLATE NAME"
        }
        _ => "ORDER BY ar.canonical_name COLLATE NAME",
    }
}

/// List every primary artist that has at least one available track in the
/// given library, with track and album counts.
#[tauri::command]
pub async fn list_artists(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListArtistsResponse> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;

    let order_clause = artist_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count,
               aw.hash                    AS artwork_hash,
               aw.format                  AS artwork_format,
               da.picture_url             AS picture_url,
               da.picture_hash            AS picture_hash
          FROM artist ar
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY ar.id
         {order_clause}
        "#
    );

    let raw = sqlx::query_as::<_, ArtistRowRaw>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&*pool)
        .await?;

    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();
    let items = expand_artist_rows(raw, artwork_dir.clone(), metadata_dir.clone()).await?;

    Ok(ListArtistsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch local + Deezer thumbnail-existence flags onto a batch of raw
/// artist rows. Same blocking-pool offload as [`expand_album_rows`]:
/// each row triggers up to 5 `Path::exists` probes (1 Deezer-full + 2
/// local thumbs + 2 Deezer thumbs) — at 900 artists that's ~4 500
/// syscalls in a tight loop, well past the threshold where stalling the
/// tokio runtime starts to matter. Shared by `list_artists` and
/// `search_artists`.
async fn expand_artist_rows(
    raw: Vec<ArtistRowRaw>,
    artwork_dir: PathBuf,
    metadata_dir: PathBuf,
) -> AppResult<Vec<ArtistRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|r| {
                let (artwork_has_1x, artwork_has_2x) = match r.artwork_hash.as_deref() {
                    Some(hash) => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                // For the Deezer cache the "full" file uses the same
                // `<hash>.jpg` naming pattern, so we can drop a `picture_hash`
                // when the source file is missing — the frontend won't have
                // anything to point a thumbnail variant at either.
                let picture_hash = r
                    .picture_hash
                    .filter(|h| crate::metadata_artwork::existing_path(&metadata_dir, h).is_some());
                let (picture_has_1x, picture_has_2x) = match picture_hash.as_deref() {
                    Some(h) => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&metadata_dir, h);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                ArtistRow {
                    id: r.id,
                    name: r.name,
                    track_count: r.track_count,
                    album_count: r.album_count,
                    artwork_hash: r.artwork_hash,
                    artwork_format: r.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    picture_hash,
                    picture_has_1x,
                    picture_has_2x,
                    picture_url: r.picture_url,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("artist row expand join: {e}")))
}

/// Search artists by name for the global top-bar search. Mirror of
/// [`search_albums`] over `artist.canonical_name`; returns the same slim
/// shape as [`list_artists`] so the frontend reuses `expandArtistRow`.
#[tauri::command]
pub async fn search_artists(
    state: tauri::State<'_, AppState>,
    query: String,
    library_id: Option<i64>,
    limit: Option<i64>,
) -> AppResult<ListArtistsResponse> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();

    let canon = waveflow_core::scanner::canonical_name(query.trim());
    if canon.is_empty() {
        return Ok(ListArtistsResponse {
            artwork_base: artwork_dir.to_string_lossy().into_owned(),
            metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
            items: Vec::new(),
        });
    }
    let limit = limit.unwrap_or(8).clamp(1, 50);

    let raw = sqlx::query_as::<_, ArtistRowRaw>(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count,
               aw.hash                    AS artwork_hash,
               aw.format                  AS artwork_format,
               da.picture_url             AS picture_url,
               da.picture_hash            AS picture_hash
          FROM artist ar
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
           AND instr(ar.canonical_name, ?) > 0
         GROUP BY ar.id
         ORDER BY (instr(ar.canonical_name, ?) = 1) DESC,
                  ar.canonical_name COLLATE NAME
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

    let items = expand_artist_rows(raw, artwork_dir.clone(), metadata_dir.clone()).await?;

    Ok(ListArtistsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
        items,
    })
}
