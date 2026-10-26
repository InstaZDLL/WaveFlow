//! SQLite implementation of album metadata derived from scanned tracks.

use async_trait::async_trait;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::{error::CoreResult, repository::album::AlbumRepository};

#[derive(Debug, Clone)]
pub struct SqliteAlbumRepository {
    pool: SqlitePool,
}

impl SqliteAlbumRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AlbumRepository for SqliteAlbumRepository {
    async fn refresh_years_after_folder_scan(
        &self,
        folder_id: i64,
        previous_album_ids: &[i64],
    ) -> CoreResult<u64> {
        let mut album_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT album_id FROM track
              WHERE folder_id = ? AND album_id IS NOT NULL",
        )
        .bind(folder_id)
        .fetch_all(&self.pool)
        .await?;
        album_ids.extend_from_slice(previous_album_ids);
        let mut conn = self.pool.acquire().await?;
        refresh_album_years(&mut conn, &album_ids).await
    }
}

/// Recompute `album.year` for these albums from their tracks: the year of
/// the first available track in disc / track order that has one. Shared
/// by the folder scan and by the tag editor, which changes a track's year
/// (or moves it to another album) outside any scan. Takes a connection so
/// the editor can run it inside its own transaction.
pub async fn refresh_album_years(
    conn: &mut sqlx::SqliteConnection,
    album_ids: &[i64],
) -> CoreResult<u64> {
    let mut album_ids = album_ids.to_vec();
    album_ids.sort_unstable();
    album_ids.dedup();

    let mut changed = 0;
    // Leave room under SQLite's bind limit for every album id.
    for chunk in album_ids.chunks(900) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH chosen AS (
                    SELECT al.id AS album_id,
                           (SELECT t.year FROM track t
                             WHERE t.album_id = al.id
                               AND t.is_available = 1
                               AND t.year IS NOT NULL
                             ORDER BY COALESCE(t.disc_number, 1),
                                      COALESCE(t.track_number, 2147483647),
                                      t.file_path COLLATE NOCASE, t.id
                             LIMIT 1) AS year
                      FROM album al
                     WHERE al.id IN (",
        );
        {
            let mut ids = query.separated(", ");
            for id in chunk {
                ids.push_bind(id);
            }
        }
        query.push(
            "))
                UPDATE album
                   SET year = chosen.year
                  FROM chosen
                 WHERE album.id = chosen.album_id
                   AND album.year IS NOT chosen.year",
        );
        changed += query.build().execute(&mut *conn).await?.rows_affected();
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
