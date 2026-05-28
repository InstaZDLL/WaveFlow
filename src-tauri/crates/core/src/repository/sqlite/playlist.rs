//! SQLite implementation of [`PlaylistRepository`].

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::{
    domain::playlist::Playlist,
    error::CoreResult,
    repository::playlist::{PlaylistDraft, PlaylistRepository, PlaylistUpdate},
};

#[derive(Debug, Clone)]
pub struct SqlitePlaylistRepository {
    pool: SqlitePool,
}

impl SqlitePlaylistRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// `SELECT … FROM playlist p LEFT JOIN (counts)` — kept as a const so
/// the same projection backs both list + single-row fetches.
const SELECT_WITH_COUNTS: &str = r#"
SELECT p.id, p.name, p.description, p.color_id, p.icon_id,
       p.is_smart, p.cover_hash, NULL AS cover_path,
       p.cover_is_auto,
       p.position, p.created_at, p.updated_at,
       COALESCE(pc.track_count,       0) AS track_count,
       COALESCE(pc.total_duration_ms, 0) AS total_duration_ms,
       p.smart_rules
  FROM playlist p
  LEFT JOIN (
      SELECT pt.playlist_id,
             COUNT(*)                AS track_count,
             SUM(t.duration_ms)      AS total_duration_ms
        FROM playlist_track pt
        JOIN track t ON t.id = pt.track_id
       WHERE t.is_available = 1
       GROUP BY pt.playlist_id
  ) pc ON pc.playlist_id = p.id
"#;

#[async_trait]
impl PlaylistRepository for SqlitePlaylistRepository {
    async fn list_all_with_counts(&self) -> CoreResult<Vec<Playlist>> {
        let sql = format!("{SELECT_WITH_COUNTS} ORDER BY p.position ASC, p.updated_at DESC");
        let playlists = sqlx::query_as::<_, Playlist>(sqlx::AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await?;
        Ok(playlists)
    }

    async fn get_with_counts(&self, id: i64) -> CoreResult<Option<Playlist>> {
        let sql = format!("{SELECT_WITH_COUNTS} WHERE p.id = ?");
        let row = sqlx::query_as::<_, Playlist>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    async fn get_name(&self, id: i64) -> CoreResult<Option<String>> {
        let name: Option<String> = sqlx::query_scalar("SELECT name FROM playlist WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(name)
    }

    async fn exists(&self, id: i64) -> CoreResult<bool> {
        let row: Option<i64> = sqlx::query_scalar("SELECT id FROM playlist WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    async fn insert_custom(&self, draft: &PlaylistDraft) -> CoreResult<i64> {
        let insert = sqlx::query(
            "INSERT INTO playlist
                 (name, description, color_id, icon_id, is_smart, position,
                  created_at, updated_at)
             VALUES (?, ?, ?, ?, 0, 0, ?, ?)",
        )
        .bind(&draft.name)
        .bind(draft.description.as_deref())
        .bind(&draft.color_id)
        .bind(&draft.icon_id)
        .bind(draft.now_ms)
        .bind(draft.now_ms)
        .execute(&self.pool)
        .await?;
        Ok(insert.last_insert_rowid())
    }

    async fn update(&self, id: i64, patch: &PlaylistUpdate, now_ms: i64) -> CoreResult<()> {
        sqlx::query(
            "UPDATE playlist
                SET name        = COALESCE(?, name),
                    description = COALESCE(?, description),
                    color_id    = COALESCE(?, color_id),
                    icon_id     = COALESCE(?, icon_id),
                    updated_at  = ?
              WHERE id = ?",
        )
        .bind(patch.name.as_deref())
        .bind(patch.description.as_deref())
        .bind(patch.color_id.as_deref())
        .bind(patch.icon_id.as_deref())
        .bind(now_ms)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self, id: i64) -> CoreResult<bool> {
        let result = sqlx::query("DELETE FROM playlist WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn touch_updated_at(&self, id: i64, now_ms: i64) -> CoreResult<()> {
        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_user_playlists_containing(&self, track_id: i64) -> CoreResult<Vec<i64>> {
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT pt.playlist_id
               FROM playlist_track pt
               JOIN playlist p ON p.id = pt.playlist_id
              WHERE pt.track_id = ?
                AND p.is_smart = 0",
        )
        .bind(track_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn append_track(&self, playlist_id: i64, track_id: i64, now_ms: i64) -> CoreResult<()> {
        // Compute next position in a single query so concurrent inserts from
        // different callers don't collide. Sqlite serializes writes at the
        // connection level, which is enough here.
        sqlx::query(
            "INSERT OR IGNORE INTO playlist_track (playlist_id, track_id, position, added_at)
             VALUES (?, ?,
                     (SELECT COALESCE(MAX(position), -1) + 1
                        FROM playlist_track
                       WHERE playlist_id = ?),
                     ?)",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(playlist_id)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;

        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(playlist_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn append_tracks(
        &self,
        playlist_id: i64,
        track_ids: &[i64],
        now_ms: i64,
    ) -> CoreResult<u32> {
        if track_ids.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let current_max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(position) FROM playlist_track WHERE playlist_id = ?")
                .bind(playlist_id)
                .fetch_one(&mut *tx)
                .await?;
        let mut next_position = current_max.map(|p| p + 1).unwrap_or(0);
        let mut inserted: u32 = 0;

        for track_id in track_ids {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO playlist_track (playlist_id, track_id, position, added_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(playlist_id)
            .bind(*track_id)
            .bind(next_position)
            .bind(now_ms)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() > 0 {
                inserted += 1;
                next_position += 1;
            }
        }

        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(playlist_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn remove_track(&self, playlist_id: i64, track_id: i64, now_ms: i64) -> CoreResult<bool> {
        // Lookup + delete + renumber must run in the same transaction so a
        // concurrent remove_track/reorder_track can't shift positions
        // between the SELECT and the position-shifting UPDATE below.
        let mut tx = self.pool.begin().await?;
        let removed_position: Option<i64> = sqlx::query_scalar(
            "SELECT position FROM playlist_track WHERE playlist_id = ? AND track_id = ?",
        )
        .bind(playlist_id)
        .bind(track_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(pos) = removed_position else {
            tx.commit().await?;
            return Ok(false);
        };

        sqlx::query("DELETE FROM playlist_track WHERE playlist_id = ? AND track_id = ?")
            .bind(playlist_id)
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE playlist_track
                SET position = position - 1
              WHERE playlist_id = ? AND position > ?",
        )
        .bind(playlist_id)
        .bind(pos)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(playlist_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn reorder_track(
        &self,
        playlist_id: i64,
        track_id: i64,
        new_position: i64,
        now_ms: i64,
    ) -> CoreResult<bool> {
        let mut tx = self.pool.begin().await?;

        let from: Option<i64> = sqlx::query_scalar(
            "SELECT position FROM playlist_track WHERE playlist_id = ? AND track_id = ?",
        )
        .bind(playlist_id)
        .bind(track_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(from) = from else {
            tx.commit().await?;
            return Ok(false);
        };

        let len: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM playlist_track WHERE playlist_id = ?")
                .bind(playlist_id)
                .fetch_one(&mut *tx)
                .await?;
        let to = new_position.clamp(0, (len - 1).max(0));

        if from == to {
            tx.commit().await?;
            return Ok(true);
        }

        if to > from {
            sqlx::query(
                "UPDATE playlist_track
                    SET position = position - 1
                  WHERE playlist_id = ? AND position > ? AND position <= ?",
            )
            .bind(playlist_id)
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "UPDATE playlist_track
                    SET position = position + 1
                  WHERE playlist_id = ? AND position >= ? AND position < ?",
            )
            .bind(playlist_id)
            .bind(to)
            .bind(from)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "UPDATE playlist_track SET position = ?
              WHERE playlist_id = ? AND track_id = ?",
        )
        .bind(to)
        .bind(playlist_id)
        .bind(track_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(playlist_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    async fn create_with_tracks(
        &self,
        draft: &PlaylistDraft,
        track_ids: &[i64],
    ) -> CoreResult<(i64, u32)> {
        let mut tx = self.pool.begin().await?;
        let insert = sqlx::query(
            "INSERT INTO playlist
                 (name, description, color_id, icon_id, is_smart, position,
                  created_at, updated_at)
             VALUES (?, ?, ?, ?, 0, 0, ?, ?)",
        )
        .bind(&draft.name)
        .bind(draft.description.as_deref())
        .bind(&draft.color_id)
        .bind(&draft.icon_id)
        .bind(draft.now_ms)
        .bind(draft.now_ms)
        .execute(&mut *tx)
        .await?;
        let new_id = insert.last_insert_rowid();

        let mut imported: u32 = 0;
        let mut next_position: i64 = 0;
        for track_id in track_ids {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO playlist_track (playlist_id, track_id, position, added_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(new_id)
            .bind(*track_id)
            .bind(next_position)
            .bind(draft.now_ms)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() > 0 {
                imported += 1;
                next_position += 1;
            }
        }
        tx.commit().await?;
        Ok((new_id, imported))
    }
}
