//! The library's artist list, from the device and from the bound server.

use std::path::PathBuf;

use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

/// One artist of the library, whichever source they come from. Same shape
/// contract as [`LibraryAlbumRow`]: text identifier, `source` says how to read
/// it.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryArtistRow {
    pub source: String,
    pub id: String,
    pub name: String,
    pub track_count: i64,
    pub album_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    pub picture_hash: Option<String>,
    pub picture_has_1x: bool,
    pub picture_has_2x: bool,
    pub picture_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListLibraryArtistsResponse {
    pub artwork_base: String,
    pub metadata_artwork_base: String,
    pub items: Vec<LibraryArtistRow>,
}

#[derive(sqlx::FromRow)]
struct LibraryArtistRawRow {
    source: String,
    id: String,
    name: String,
    track_count: i64,
    album_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    picture_url: Option<String>,
    picture_hash: Option<String>,
}

/// Ordering for the unified artist listing — on the union's own columns, for
/// the reason spelled out on [`library_album_order_clause`].
pub(super) fn library_artist_order_clause(
    order_by: Option<&str>,
    direction: Option<&str>,
) -> &'static str {
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
        (Some("name"), "ASC") => "ORDER BY sort_name COLLATE NAME ASC",
        (Some("name"), "DESC") => "ORDER BY sort_name COLLATE NAME DESC",
        (Some("albums_count"), "ASC") => "ORDER BY album_count ASC, sort_name COLLATE NAME",
        (Some("albums_count"), "DESC") => "ORDER BY album_count DESC, sort_name COLLATE NAME",
        (Some("tracks_count"), "ASC") => "ORDER BY track_count ASC, sort_name COLLATE NAME",
        (Some("tracks_count"), "DESC") => "ORDER BY track_count DESC, sort_name COLLATE NAME",
        _ => "ORDER BY sort_name COLLATE NAME",
    }
}

/// Every artist the library can show, from the device and from the bound
/// server, as one sorted list.
///
/// Not merged, on the same terms as the albums: an artist credited on both
/// sides appears twice. The remote half comes from the mirrored catalogue, and
/// its counts are derived from the albums and tracks already mirrored rather
/// than stored — a stored count would go stale the moment an album is walked.
#[tauri::command]
pub async fn list_library_artists(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    source: Option<String>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListLibraryArtistsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();

    let order_clause = library_artist_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = library_artists_sql(order_clause);

    let raw = sqlx::query_as::<_, LibraryArtistRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source.as_deref())
        .bind(source.as_deref())
        .fetch_all(&*pool)
        .await?;

    let items = expand_library_artist_rows(raw, artwork_dir.clone(), metadata_dir.clone()).await?;

    Ok(ListLibraryArtistsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch thumbnail-existence flags onto the local half only — a remote hash
/// names nothing in either local directory. See
/// [`expand_library_album_rows`].
async fn expand_library_artist_rows(
    raw: Vec<LibraryArtistRawRow>,
    artwork_dir: PathBuf,
    metadata_dir: PathBuf,
) -> AppResult<Vec<LibraryArtistRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|r| {
                let local = r.source == "local";
                let (artwork_has_1x, artwork_has_2x) = match r.artwork_hash.as_deref() {
                    Some(hash) if local => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    _ => (false, false),
                };
                let (picture_has_1x, picture_has_2x) = match r.picture_hash.as_deref() {
                    Some(hash) if local => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&metadata_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    _ => (false, false),
                };
                LibraryArtistRow {
                    source: r.source,
                    id: r.id,
                    name: r.name,
                    track_count: r.track_count,
                    album_count: r.album_count,
                    artwork_hash: r.artwork_hash,
                    artwork_format: r.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    picture_hash: r.picture_hash,
                    picture_has_1x,
                    picture_has_2x,
                    picture_url: r.picture_url,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("library artist row expand join: {e}")))
}
