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
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
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
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// The real migrator, `foreign_keys` on — the cascade and the CHECK are
    /// the point of the table, and a hand-written fixture would have
    /// neither. The listing joins the attached `app` database, so the
    /// fixture attaches one with the single table it reads.
    async fn pool() -> SqlitePool {
        let options = waveflow_core::repository::sqlite::collation::register(
            SqliteConnectOptions::from_str(":memory:")
                .unwrap()
                .foreign_keys(true),
        );
        let pool = SqlitePoolOptions::new()
            // `ATTACH` is per connection.
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("ATTACH DATABASE ':memory:' AS app")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE app.metadata_artist (
                deezer_id    INTEGER PRIMARY KEY,
                picture_url  TEXT,
                picture_hash TEXT
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        for statement in [
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
            "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')",
            "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (1, 'One', 'one', 1, 0)",
            "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (2, 'Two', 'two', 1, 0)",
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/1.flac', 'h1', 1, 0, 'T1', 1, 1000, 0, 1, 0, 0, 0, 0)",
            "INSERT INTO track_artist (track_id, artist_id, position) VALUES (1, 1, 0)",
            "INSERT INTO playlist (id, name, created_at, updated_at) VALUES (1, 'P', 0, 0)",
        ] {
            sqlx::raw_sql(statement).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn listed(pool: &SqlitePool) -> Vec<(String, i64, String)> {
        list_rows(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.kind, r.target_id, r.name))
            .collect()
    }

    #[tokio::test]
    async fn pins_list_in_the_order_they_were_pinned() {
        let pool = pool().await;
        sidebar_pin::pin(&pool, PinKind::Playlist, 1, 10)
            .await
            .unwrap();
        sidebar_pin::pin(&pool, PinKind::Album, 1, 20)
            .await
            .unwrap();
        sidebar_pin::pin(&pool, PinKind::Artist, 1, 30)
            .await
            .unwrap();
        // A second pin of the same album keeps its first place.
        sidebar_pin::pin(&pool, PinKind::Album, 1, 40)
            .await
            .unwrap();
        assert_eq!(
            listed(&pool).await,
            vec![
                ("playlist".into(), 1, "P".into()),
                ("album".into(), 1, "One".into()),
                ("artist".into(), 1, "A".into()),
            ]
        );
        let row = list_rows(&pool).await.unwrap().remove(1);
        assert_eq!(row.subtitle.as_deref(), Some("A"));
    }

    #[tokio::test]
    async fn unpin_removes_only_that_pin() {
        let pool = pool().await;
        sidebar_pin::pin(&pool, PinKind::Album, 1, 1).await.unwrap();
        sidebar_pin::pin(&pool, PinKind::Playlist, 1, 2)
            .await
            .unwrap();
        assert!(sidebar_pin::unpin(&pool, PinKind::Album, 1).await.unwrap());
        assert!(!sidebar_pin::unpin(&pool, PinKind::Album, 1).await.unwrap());
        assert_eq!(
            listed(&pool).await,
            vec![("playlist".into(), 1, "P".into())]
        );
    }

    #[tokio::test]
    async fn a_deleted_playlist_takes_its_pin_with_it() {
        let pool = pool().await;
        sidebar_pin::pin(&pool, PinKind::Playlist, 1, 1)
            .await
            .unwrap();
        sqlx::query("DELETE FROM playlist WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sidebar_pin")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn an_album_without_available_tracks_is_hidden_not_unpinned() {
        let pool = pool().await;
        // Album 2 has no track at all.
        sidebar_pin::pin(&pool, PinKind::Album, 2, 1).await.unwrap();
        assert!(listed(&pool).await.is_empty());
        sqlx::query("UPDATE track SET album_id = 2 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(listed(&pool).await, vec![("album".into(), 2, "Two".into())]);
        sqlx::query("UPDATE track SET is_available = 0")
            .execute(&pool)
            .await
            .unwrap();
        assert!(listed(&pool).await.is_empty());
        let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sidebar_pin")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, 1);
    }

    #[tokio::test]
    async fn a_pin_points_at_exactly_one_target() {
        let pool = pool().await;
        let both = sqlx::query(
            "INSERT INTO sidebar_pin (album_id, artist_id, pinned_at) VALUES (1, 1, 0)",
        )
        .execute(&pool)
        .await;
        assert!(both.is_err(), "the CHECK must refuse two targets");
        let missing = sidebar_pin::pin(&pool, PinKind::Album, 99, 0).await;
        assert!(
            missing.is_err(),
            "the foreign key must refuse a ghost album"
        );
    }

    #[tokio::test]
    async fn merging_an_implicit_compilation_carries_its_pin() {
        let pool = pool().await;
        // One title under three different artists, no album artist, no
        // compilation flag: the post-scan pass merges them into the
        // lowest id.
        for statement in [
            "INSERT INTO artist (id, name, canonical_name) VALUES (2, 'B', 'b')",
            "INSERT INTO artist (id, name, canonical_name) VALUES (3, 'C', 'c')",
            "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (10, 'Hits', 'hits', 1, 0)",
            "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (11, 'Hits', 'hits', 2, 0)",
            "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (12, 'Hits', 'hits', 3, 0)",
            "UPDATE track SET album_id = 11 WHERE id = 1",
        ] {
            sqlx::raw_sql(statement).execute(&pool).await.unwrap();
        }
        sidebar_pin::pin(&pool, PinKind::Album, 11, 1)
            .await
            .unwrap();
        waveflow_core::scanner::merge_implicit_compilations(&pool)
            .await
            .unwrap();
        assert_eq!(
            listed(&pool).await,
            vec![("album".into(), 10, "Hits".into())]
        );
    }
}
