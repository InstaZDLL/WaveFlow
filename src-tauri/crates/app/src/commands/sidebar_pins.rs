//! Albums, artists and playlists pinned to the sidebar.
//!
//! The rows and their writes live in
//! [`waveflow_core::repository::sqlite::sidebar_pin`]; this module resolves
//! a pin into what the sidebar draws — a name, a subtitle, a picture — and
//! hides the album and artist pins whose tracks are all gone.

use std::path::Path;

use serde::Serialize;
use sqlx::{FromRow, SqlitePool};
use waveflow_core::repository::sqlite::sidebar_pin::{self, PinKind};

use crate::error::AppResult;
use crate::state::AppState;

/// One pinned entry, as the sidebar draws it.
#[derive(Debug, Clone, Serialize)]
pub struct SidebarPin {
    pub kind: PinKind,
    pub id: i64,
    pub name: String,
    /// The album's artist; `None` for an artist or a playlist.
    pub subtitle: Option<String>,
    /// Album cover or artist picture, full path, then its thumbnails.
    pub image_path: Option<String>,
    pub image_path_1x: Option<String>,
    pub image_path_2x: Option<String>,
}

#[derive(FromRow)]
struct PinRaw {
    kind: String,
    target_id: i64,
    name: String,
    subtitle: Option<String>,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    picture_hash: Option<String>,
}

/// The pins, in the order they were pinned. An album or artist with no
/// available track is left out — its row would open on nothing — and
/// comes back with its tracks; its pin is kept meanwhile.
async fn list_rows(pool: &SqlitePool) -> AppResult<Vec<PinRaw>> {
    let rows = sqlx::query_as::<_, PinRaw>(
        r#"
        SELECT CASE WHEN p.album_id  IS NOT NULL THEN 'album'
                    WHEN p.artist_id IS NOT NULL THEN 'artist'
                    ELSE 'playlist' END                     AS kind,
               COALESCE(p.album_id, p.artist_id, p.playlist_id) AS target_id,
               COALESCE(al.title, ar.name, pl.name)         AS name,
               CASE WHEN p.album_id IS NOT NULL
                    THEN COALESCE(aa.name, al.album_artist) END AS subtitle,
               aw.hash         AS artwork_hash,
               aw.format       AS artwork_format,
               da.picture_hash AS picture_hash
          FROM sidebar_pin p
          LEFT JOIN album al    ON al.id = p.album_id
          LEFT JOIN artist aa   ON aa.id = al.artist_id
          LEFT JOIN artist ar   ON ar.id = p.artist_id
          LEFT JOIN playlist pl ON pl.id = p.playlist_id
          LEFT JOIN artwork aw  ON aw.id = COALESCE(al.artwork_id, ar.artwork_id)
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE p.playlist_id IS NOT NULL
            OR (p.album_id IS NOT NULL AND EXISTS (
                    SELECT 1 FROM track t
                     WHERE t.album_id = p.album_id AND t.is_available = 1))
            -- Credited on a track, or album artist of its album: the same
            -- reach as the artist page.
            OR (p.artist_id IS NOT NULL AND EXISTS (
                    SELECT 1 FROM track t
                     WHERE t.is_available = 1
                       AND (t.id IN (SELECT track_id FROM track_artist
                                      WHERE artist_id = p.artist_id)
                            OR t.album_id IN (SELECT id FROM album
                                               WHERE artist_id = p.artist_id))))
         ORDER BY p.pinned_at, p.id
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A row's picture: the profile's own artwork (an album cover, an
/// artist's local image) first, then an artist's cached Deezer picture.
fn resolve(row: PinRaw, artwork_dir: &Path, metadata_dir: &Path) -> Option<SidebarPin> {
    let kind = match row.kind.as_str() {
        "album" => PinKind::Album,
        "artist" => PinKind::Artist,
        "playlist" => PinKind::Playlist,
        _ => return None,
    };
    let (image_path, image_path_1x, image_path_2x) =
        match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
            (Some(hash), Some(format)) => {
                let full = artwork_dir
                    .join(format!("{hash}.{format}"))
                    .to_string_lossy()
                    .into_owned();
                let (p1, p2) = crate::thumbnails::thumbnail_paths_for(artwork_dir, hash);
                (Some(full), p1, p2)
            }
            _ => match row.picture_hash.as_deref() {
                Some(hash) => {
                    let full = crate::metadata_artwork::existing_path(metadata_dir, hash);
                    let (p1, p2) = crate::thumbnails::thumbnail_paths_for(metadata_dir, hash);
                    (full, p1, p2)
                }
                None => (None, None, None),
            },
        };
    Some(SidebarPin {
        kind,
        id: row.target_id,
        name: row.name,
        subtitle: row.subtitle,
        image_path,
        image_path_1x,
        image_path_2x,
    })
}

#[tauri::command]
pub async fn list_sidebar_pins(state: tauri::State<'_, AppState>) -> AppResult<Vec<SidebarPin>> {
    // One read of the profile: a switch between two awaits would resolve
    // one profile's artwork hashes in the other's folder.
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();
    let rows = list_rows(&pool).await?;
    // The thumbnail probes are filesystem calls; one hop to the blocking
    // pool for the whole list.
    let pins = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .filter_map(|row| resolve(row, &artwork_dir, &metadata_dir))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| crate::error::AppError::Other(format!("sidebar pins join: {e}")))?;
    Ok(pins)
}

/// Pin or unpin an album, an artist or a local playlist.
#[tauri::command]
pub async fn set_sidebar_pin(
    state: tauri::State<'_, AppState>,
    kind: PinKind,
    id: i64,
    pinned: bool,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    if pinned {
        sidebar_pin::pin(&pool, kind, id, chrono::Utc::now().timestamp_millis()).await?;
    } else {
        sidebar_pin::unpin(&pool, kind, id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
