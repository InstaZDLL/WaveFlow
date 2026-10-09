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
            changed += query.build().execute(&self.pool).await?.rows_affected();
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn album_year(pool: &SqlitePool, id: i64) -> Option<i64> {
        sqlx::query_scalar("SELECT year FROM album WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn fixture_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "CREATE TABLE album (id INTEGER PRIMARY KEY, year INTEGER);
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 album_id INTEGER,
                 folder_id INTEGER NOT NULL,
                 file_path TEXT NOT NULL,
                 disc_number INTEGER,
                 track_number INTEGER,
                 year INTEGER,
                 is_available INTEGER NOT NULL DEFAULT 1
             );",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn rescan_picks_a_stable_year_and_applies_tag_corrections() {
        let pool = fixture_pool().await;
        sqlx::raw_sql(
            "INSERT INTO album (id, year) VALUES (1, NULL), (2, 1999);
             INSERT INTO track (id, album_id, folder_id, file_path, track_number, year)
             VALUES (2, 1, 1, '02.mp3', 2, 2025),
                    (1, 1, 1, '01.mp3', 1, 2024),
                    (3, 2, 2, 'other.mp3', 1, 2000);",
        )
        .execute(&pool)
        .await
        .unwrap();
        let repo = SqliteAlbumRepository::new(pool.clone());

        assert_eq!(
            repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
            1
        );
        assert_eq!(album_year(&pool, 1).await, Some(2024));
        assert_eq!(album_year(&pool, 2).await, Some(1999));

        sqlx::query("UPDATE track SET year = 2026 WHERE album_id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
            1
        );
        assert_eq!(album_year(&pool, 1).await, Some(2026));

        sqlx::query("UPDATE track SET year = NULL WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
            0
        );
        assert_eq!(album_year(&pool, 1).await, Some(2026));

        sqlx::query("UPDATE track SET year = NULL WHERE id = 2")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            repo.refresh_years_after_folder_scan(1, &[]).await.unwrap(),
            1
        );
        assert_eq!(album_year(&pool, 1).await, None);
    }

    #[tokio::test]
    async fn reassignment_updates_the_former_album_in_another_folder() {
        let pool = fixture_pool().await;
        sqlx::raw_sql(
            "INSERT INTO album (id, year) VALUES (1, 2024), (2, 2025);
             INSERT INTO track (id, album_id, folder_id, file_path, track_number, year)
             VALUES (1, 1, 2, 'moved.mp3', 1, 2024),
                    (2, 1, 1, 'remaining.mp3', 2, 2020),
                    (3, 2, 2, 'other.mp3', 2, 2025);",
        )
        .execute(&pool)
        .await
        .unwrap();
        let repo = SqliteAlbumRepository::new(pool.clone());

        // Retagging a file in folder 2 moves it to album 2. Album 1
        // still has a track in folder 1, but none in the scanned folder.
        sqlx::query("UPDATE track SET album_id = 2 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            repo.refresh_years_after_folder_scan(2, &[1]).await.unwrap(),
            2
        );
        assert_eq!(album_year(&pool, 1).await, Some(2020));
        assert_eq!(album_year(&pool, 2).await, Some(2024));
    }
}
