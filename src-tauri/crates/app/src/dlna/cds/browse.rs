//! The listing queries behind each container: artists, albums, genres,
//! playlists, library folders and the tracks inside them.

use sqlx::SqlitePool;

use crate::dlna::http::ServerCtx;

use super::didl::{container_xml, didl_envelope, item_xml};
use super::{
    containers_page, empty_result, items_result, BrowseResult, FolderDir, ObjectId, TrackRow,
};

pub(super) async fn browse_artists(
    pool: &SqlitePool,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artist")
        .fetch_one(pool)
        .await?;
    // The per-row counts ride `idx_album_artist`, so a page of 500
    // stays a page of index probes.
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT ar.id, ar.name,
                (SELECT COUNT(*) FROM album al WHERE al.artist_id = ar.id)
           FROM artist ar
          ORDER BY ar.canonical_name
          LIMIT ? OFFSET ?",
    )
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(containers_page("0/artists", &rows, total, |id| {
        ObjectId::Artist(id).to_id()
    }))
}

pub(super) async fn browse_artist_albums(
    pool: &SqlitePool,
    artist_id: i64,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album WHERE artist_id = ?")
        .bind(artist_id)
        .fetch_one(pool)
        .await?;
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT al.id, al.title,
                (SELECT COUNT(*) FROM track t WHERE t.album_id = al.id AND t.is_available = 1)
           FROM album al
          WHERE al.artist_id = ?
          ORDER BY al.year, al.canonical_title, al.id
          LIMIT ? OFFSET ?",
    )
    .bind(artist_id)
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(containers_page(
        &ObjectId::Artist(artist_id).to_id(),
        &rows,
        total,
        |id| ObjectId::Album(id).to_id(),
    ))
}

pub(super) async fn browse_albums(
    pool: &SqlitePool,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album")
        .fetch_one(pool)
        .await?;
    // Only available tracks count, because `browse_album_tracks` lists
    // only those; the per-row count rides `idx_track_album_available`.
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT al.id, al.title,
                (SELECT COUNT(*) FROM track t WHERE t.album_id = al.id AND t.is_available = 1)
           FROM album al
          ORDER BY al.canonical_title, al.id
          LIMIT ? OFFSET ?",
    )
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(containers_page("0/albums", &rows, total, |id| {
        ObjectId::Album(id).to_id()
    }))
}

pub(super) async fn browse_genres(
    pool: &SqlitePool,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar(concat!(
        "SELECT COUNT(*) FROM (",
        genre_rows!(),
        ") WHERE tracks > 0"
    ))
    .fetch_one(pool)
    .await?;
    // The per-row counts ride `idx_track_genre_genre`.
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(concat!(
        "SELECT id, name, tracks FROM (",
        genre_rows!(),
        ") WHERE tracks > 0
          ORDER BY name COLLATE FOLD, id
          LIMIT ? OFFSET ?"
    ))
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(containers_page("0/genres", &rows, total, |id| {
        ObjectId::Genre(id).to_id()
    }))
}

pub(super) async fn browse_playlists(
    pool: &SqlitePool,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist")
        .fetch_one(pool)
        .await?;
    // The sidebar's order, with the id last so a page boundary never
    // falls between two rows that compare equal.
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(concat!(
        "SELECT id, name, tracks FROM (",
        playlist_rows!(),
        ") ORDER BY position, updated_at DESC, id
          LIMIT ? OFFSET ?"
    ))
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(containers_page("0/playlists", &rows, total, |id| {
        ObjectId::Playlist(id).to_id()
    }))
}

/// The directory `rel` below the `library_folder` row `root`, or `None`
/// when that row is gone.
async fn load_folder(
    pool: &SqlitePool,
    root: i64,
    rel: &[String],
) -> Result<Option<FolderDir>, sqlx::Error> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT library_id, path FROM library_folder WHERE id = ?")
            .bind(root)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(library_id, path)| FolderDir::new(library_id, &path, rel)))
}

/// The directories directly inside a directory, each with the number
/// of entries (directories and files) directly inside it.
///
/// Derived from `track.file_path` at query time, as the app's own
/// folder browser does: the band scan walks the
/// `(library_id, file_path)` index, so the cost is the directory's own
/// subtree. A directory holding no available track does not exist
/// here. Names group byte for byte: two directories that differ only
/// in case are two directories wherever the filesystem says so.
const FOLDER_SUBDIRS_SQL: &str = r#"
    WITH child AS (
      SELECT substr(rest, 1, instr(rest, ?) - 1) AS name,
             substr(rest, instr(rest, ?) + 1)     AS below
        FROM (SELECT substr(file_path, ? + 1) AS rest
                FROM track
               WHERE library_id = ?
                 AND is_available = 1
                 AND file_path >= ?
                 AND file_path <  ?)
       WHERE instr(rest, ?) > 0
    )
    SELECT name,
           COUNT(DISTINCT CASE WHEN instr(below, ?) > 0
                               THEN substr(below, 1, instr(below, ?) - 1)
                               ELSE below END) AS children
      FROM child
     GROUP BY name
     ORDER BY name COLLATE FOLD, name
"#;

async fn folder_subdirs(
    pool: &SqlitePool,
    dir: &FolderDir,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let sep = std::path::MAIN_SEPARATOR.to_string();
    sqlx::query_as(FOLDER_SUBDIRS_SQL)
        .bind(&sep)
        .bind(&sep)
        .bind(dir.prefix_chars())
        .bind(dir.library_id)
        .bind(&dir.low)
        .bind(&dir.high)
        .bind(&sep)
        .bind(&sep)
        .bind(&sep)
        .fetch_all(pool)
        .await
}

/// The available files directly inside a directory.
async fn folder_file_count(pool: &SqlitePool, dir: &FolderDir) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM track
          WHERE library_id = ?
            AND is_available = 1
            AND file_path >= ?
            AND file_path <  ?
            AND instr(substr(file_path, ? + 1), ?) = 0",
    )
    .bind(dir.library_id)
    .bind(&dir.low)
    .bind(&dir.high)
    .bind(dir.prefix_chars())
    .bind(std::path::MAIN_SEPARATOR.to_string())
    .fetch_one(pool)
    .await
}

/// A directory's `childCount`: its subdirectories plus its files.
pub(super) async fn folder_child_count(
    pool: &SqlitePool,
    dir: &FolderDir,
) -> Result<i64, sqlx::Error> {
    Ok(folder_subdirs(pool, dir).await?.len() as i64 + folder_file_count(pool, dir).await?)
}

/// The library folders. Each is titled with its full path: two roots
/// can share a last name (`/a/Music`, `/b/Music`), and a controller
/// shows nothing else to tell them apart.
pub(super) async fn browse_library_folders(
    pool: &SqlitePool,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_folder")
        .fetch_one(pool)
        .await?;
    let roots: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT id, library_id, path
           FROM library_folder
          ORDER BY path COLLATE FOLD, id
          LIMIT ? OFFSET ?",
    )
    .bind(count)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let mut rows = Vec::with_capacity(roots.len());
    for (id, library_id, path) in roots {
        let children = folder_child_count(pool, &FolderDir::new(library_id, &path, &[])).await?;
        rows.push((id, path, children));
    }
    Ok(containers_page("0/folders", &rows, total, |id| {
        ObjectId::Folder {
            root: id,
            rel: Vec::new(),
        }
        .to_id()
    }))
}

/// One directory: its subdirectories first, then its files, as one
/// list the page is cut from.
pub(super) async fn browse_folder(
    ctx: &ServerCtx,
    base: &str,
    root: i64,
    rel: &[String],
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let Some(dir) = load_folder(&ctx.pool, root, rel).await? else {
        return Ok(empty_result());
    };
    let subdirs = folder_subdirs(&ctx.pool, &dir).await?;
    let files = folder_file_count(&ctx.pool, &dir).await?;
    let dir_count = subdirs.len() as i64;
    let parent = ObjectId::Folder {
        root,
        rel: rel.to_vec(),
    }
    .to_id();

    let mut body = String::new();
    let skip = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = usize::try_from(count).unwrap_or(0);
    let mut returned = 0;
    for (name, children) in subdirs.iter().skip(skip).take(take) {
        let mut child_rel = rel.to_vec();
        child_rel.push(name.clone());
        let id = ObjectId::Folder {
            root,
            rel: child_rel,
        }
        .to_id();
        body.push_str(&container_xml(&id, &parent, name, Some(*children)));
        returned += 1;
    }

    let file_count = count - returned;
    if file_count > 0 {
        let file_offset = (offset - dir_count).max(0);
        let rows: Vec<TrackRow> = sqlx::query_as(track_select!(
            "WHERE t.library_id = ?
               AND t.is_available = 1
               AND t.file_path >= ?
               AND t.file_path <  ?
               AND instr(substr(t.file_path, ? + 1), ?) = 0
             ORDER BY t.file_path COLLATE FOLD, t.id
             LIMIT ? OFFSET ?"
        ))
        .bind(dir.library_id)
        .bind(&dir.low)
        .bind(&dir.high)
        .bind(dir.prefix_chars())
        .bind(std::path::MAIN_SEPARATOR.to_string())
        .bind(file_count)
        .bind(file_offset)
        .fetch_all(&ctx.pool)
        .await?;
        for row in &rows {
            body.push_str(&item_xml(base, row, &parent));
        }
        returned += rows.len() as i64;
    }

    Ok(BrowseResult {
        didl: didl_envelope(&body),
        number_returned: returned,
        total_matches: dir_count + files,
    })
}

pub(super) async fn browse_album_tracks(
    ctx: &ServerCtx,
    base: &str,
    album_id: i64,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE album_id = ? AND is_available = 1")
            .bind(album_id)
            .fetch_one(&ctx.pool)
            .await?;
    let rows: Vec<TrackRow> = sqlx::query_as(track_select!(
        "WHERE t.album_id = ? AND t.is_available = 1
         ORDER BY t.disc_number, t.track_number, t.title, t.id
         LIMIT ? OFFSET ?"
    ))
    .bind(album_id)
    .bind(count)
    .bind(offset)
    .fetch_all(&ctx.pool)
    .await?;
    Ok(items_result(
        base,
        &ObjectId::Album(album_id).to_id(),
        &rows,
        total,
    ))
}

/// A genre's tracks, grouped as a shelf is: by album artist, then
/// album, then running order, so queueing the whole genre plays albums
/// through rather than shuffling them. The album's artist, not the
/// track's: a compilation's tracks each name a different one and would
/// scatter among them. The album id keeps two albums that share a
/// title (an artist's own "Greatest Hits" and a compilation of the same
/// name) apart.
pub(super) async fn browse_genre_tracks(
    ctx: &ServerCtx,
    base: &str,
    genre_id: i64,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM track_genre tg
           JOIN track t ON t.id = tg.track_id
          WHERE tg.genre_id = ? AND t.is_available = 1",
    )
    .bind(genre_id)
    .fetch_one(&ctx.pool)
    .await?;
    let rows: Vec<TrackRow> = sqlx::query_as(track_select!(
        "JOIN track_genre tg ON tg.track_id = t.id
         LEFT JOIN artist aa ON aa.id = al.artist_id
         WHERE tg.genre_id = ? AND t.is_available = 1
         ORDER BY aa.canonical_name, al.canonical_title, al.id,
                  t.disc_number, t.track_number, t.id
         LIMIT ? OFFSET ?"
    ))
    .bind(genre_id)
    .bind(count)
    .bind(offset)
    .fetch_all(&ctx.pool)
    .await?;
    Ok(items_result(
        base,
        &ObjectId::Genre(genre_id).to_id(),
        &rows,
        total,
    ))
}

/// A playlist's tracks in its own order.
pub(super) async fn browse_playlist_tracks(
    ctx: &ServerCtx,
    base: &str,
    playlist_id: i64,
    count: i64,
    offset: i64,
) -> Result<BrowseResult, sqlx::Error> {
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM playlist_track pt
           JOIN track t ON t.id = pt.track_id
          WHERE pt.playlist_id = ? AND t.is_available = 1",
    )
    .bind(playlist_id)
    .fetch_one(&ctx.pool)
    .await?;
    let rows: Vec<TrackRow> = sqlx::query_as(track_select!(
        "JOIN playlist_track pt ON pt.track_id = t.id
         WHERE pt.playlist_id = ? AND t.is_available = 1
         ORDER BY pt.position, t.id
         LIMIT ? OFFSET ?"
    ))
    .bind(playlist_id)
    .bind(count)
    .bind(offset)
    .fetch_all(&ctx.pool)
    .await?;
    Ok(items_result(
        base,
        &ObjectId::Playlist(playlist_id).to_id(),
        &rows,
        total,
    ))
}
