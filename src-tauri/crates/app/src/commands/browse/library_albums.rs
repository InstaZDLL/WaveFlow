//! The library's album list, from the device and from the bound server.

use std::path::PathBuf;

use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

/// One album of the library, whichever source it comes from.
///
/// Deliberately not an [`AlbumRow`]: the two sources do not share an
/// identifier type. A local album is a rowid, a server album is a UUID, and
/// widening the local one to a string across every existing consumer would
/// be a large change to say something small. Here the identifier is text and
/// `source` says how to read it.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryAlbumRow {
    /// `"local"` or `"remote"`. The discriminant, not decoration: it decides
    /// how `id` is resolved, where the cover comes from, and which detail
    /// view a click opens.
    pub source: String,
    /// Local rowid rendered as text, or the server's album UUID.
    pub id: String,
    pub title: String,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    /// Content hash of the cover. A local hash names a file in the profile's
    /// artwork directory; a remote one is resolved through the server's cover
    /// cache. Same field, two resolutions — hence `source`.
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    pub max_bit_depth: Option<i64>,
    pub max_sample_rate: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListLibraryAlbumsResponse {
    pub artwork_base: String,
    pub items: Vec<LibraryAlbumRow>,
}

#[derive(sqlx::FromRow)]
struct LibraryAlbumRawRow {
    source: String,
    id: String,
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

/// Ordering for the unified listing.
///
/// Separate from [`album_order_clause`] because it has to be: that one sorts
/// on the inner tables' own columns (`al.canonical_title`, `MIN(t.added_at)`),
/// which do not exist outside the local half of the union. This one sorts on
/// the columns the union itself projects, so both halves obey one comparison
/// and one collation — a list sorted differently depending on which source a
/// row came from would be worse than two lists.
pub(super) fn library_album_order_clause(
    order_by: Option<&str>,
    direction: Option<&str>,
) -> &'static str {
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
        (Some("title"), "ASC") => "ORDER BY sort_title COLLATE NAME ASC",
        (Some("title"), "DESC") => "ORDER BY sort_title COLLATE NAME DESC",
        (Some("artist"), "ASC") => "ORDER BY sort_artist COLLATE NAME ASC, sort_title COLLATE NAME",
        (Some("artist"), "DESC") => {
            "ORDER BY sort_artist COLLATE NAME DESC, sort_title COLLATE NAME"
        }
        (Some("year"), "ASC") => "ORDER BY year ASC, sort_title COLLATE NAME",
        (Some("year"), "DESC") => "ORDER BY year DESC, sort_title COLLATE NAME",
        (Some("added_at"), "ASC") => "ORDER BY added_at ASC",
        (Some("added_at"), "DESC") => "ORDER BY added_at DESC",
        _ => "ORDER BY sort_artist COLLATE NAME, sort_title COLLATE NAME",
    }
}

/// Every album the library can show, from the device and from the bound
/// server, as one sorted list.
///
/// The two halves are **not** merged: an album held both locally and on the
/// server appears twice, tagged twice, which is what RFC-005 decision 1 says
/// and what the source chip explains. Unifying the navigation is not
/// deduplicating the catalogue.
///
/// `source` filters the list to one half; `None` means both. The remote half
/// comes from the mirrored catalogue, so it is exactly as complete as the
/// last walk left it — and empty, at no cost, on a build without `sync_v2`.
#[tauri::command]
pub async fn list_library_albums(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    source: Option<String>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListLibraryAlbumsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = library_album_order_clause(order_by.as_deref(), direction.as_deref());

    // The sort keys are projected rather than computed in the ORDER BY: the
    // two halves spell them differently (`canonical_title` against
    // `sort_name`) and only the aliases exist outside the union.
    let sql = library_albums_sql(order_clause);

    let raw = sqlx::query_as::<_, LibraryAlbumRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source.as_deref())
        .bind(source.as_deref())
        .fetch_all(&*pool)
        .await?;

    let items = expand_library_album_rows(raw, artwork_dir.clone()).await?;

    Ok(ListLibraryAlbumsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch thumbnail-existence flags onto the local half only.
///
/// A remote hash names nothing in the profile's artwork directory, so probing
/// for it would be `is_file` calls guaranteed to fail — once per remote album,
/// twice each, on the blocking pool. The frontend resolves those covers
/// through the server cover cache instead.
async fn expand_library_album_rows(
    raw: Vec<LibraryAlbumRawRow>,
    artwork_dir: PathBuf,
) -> AppResult<Vec<LibraryAlbumRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|row| {
                let local = row.source == "local";
                let (artwork_has_1x, artwork_has_2x) = match row.artwork_hash.as_deref() {
                    Some(hash) if local => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    _ => (false, false),
                };
                LibraryAlbumRow {
                    source: row.source,
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
    .map_err(|e| AppError::Other(format!("library album row expand join: {e}")))
}
