//! SQLite implementation of [`LibraryRepository`].
//!
//! Same two-flavour shape as the playlist repo: a trait impl on
//! [`SqliteLibraryRepository`] for legacy callers, plus free
//! `*_conn` helpers that take `&mut SqliteConnection` so the
//! desktop's CRUD commands can compose the library write + an
//! `enqueue_op_in_tx` outbox row + a `set_canonical_library`
//! mapping update inside a single transaction. The trait methods
//! just `pool.acquire()` + delegate.

use async_trait::async_trait;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    domain::library::{Library, LibraryFolder},
    error::CoreResult,
    repository::library::{LibraryDraft, LibraryRepository, LibraryUpdate},
};

// ── Connection-taking helpers ───────────────────────────────────

/// Insert a new library row. Returns the assigned rowid.
pub async fn insert_conn(conn: &mut SqliteConnection, draft: &LibraryDraft) -> CoreResult<i64> {
    let insert = sqlx::query(
        "INSERT INTO library (name, description, color_id, icon_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&draft.name)
    .bind(draft.description.as_deref())
    .bind(&draft.color_id)
    .bind(&draft.icon_id)
    .bind(draft.now_ms)
    .bind(draft.now_ms)
    .execute(conn)
    .await?;
    Ok(insert.last_insert_rowid())
}

/// Partial `UPDATE library` via COALESCE. Returns `true` when a row
/// was actually touched. Mirrors the playlist `update_conn` shape so
/// the desktop's tx-aware caller can collapse the pre-tx exists()
/// probe into the same transaction as the update + outbox enqueue.
pub async fn update_conn(
    conn: &mut SqliteConnection,
    id: i64,
    patch: &LibraryUpdate,
    now_ms: i64,
) -> CoreResult<bool> {
    let res = sqlx::query(
        "UPDATE library
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
    .execute(conn)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Delete a library. `true` when a row was deleted, `false` when no
/// row matched. Cascades through `library_folder`, `track`, …
pub async fn delete_conn(conn: &mut SqliteConnection, id: i64) -> CoreResult<bool> {
    let result = sqlx::query("DELETE FROM library WHERE id = ?")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[derive(Debug, Clone)]
pub struct SqliteLibraryRepository {
    pool: SqlitePool,
}

impl SqliteLibraryRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl LibraryRepository for SqliteLibraryRepository {
    async fn list_all_with_counts(&self) -> CoreResult<Vec<Library>> {
        // A single query with LEFT JOIN + COUNT(DISTINCT ...) keeps ordering stable
        // and avoids the N+1 problem when rendering the sidebar.
        let libraries = sqlx::query_as::<_, Library>(
            r#"
            SELECT l.id, l.name, l.description, l.color_id, l.icon_id,
                   l.created_at, l.updated_at,
                   COALESCE(tc.track_count,  0) AS track_count,
                   COALESCE(tc.album_count,  0) AS album_count,
                   COALESCE(tc.artist_count, 0) AS artist_count,
                   COALESCE(gc.genre_count,  0) AS genre_count,
                   COALESCE(f.folder_count,  0) AS folder_count
              FROM library l
              LEFT JOIN (
                  SELECT t.library_id,
                         COUNT(DISTINCT t.id)             AS track_count,
                         COUNT(DISTINCT t.album_id)       AS album_count,
                         -- Join to `track_artist` so a track with several
                         -- contributing artists (multi-artist split) is
                         -- counted under each of them. The earlier
                         -- `COUNT(DISTINCT primary_artist)` only saw the
                         -- first artist of each track and under-reported
                         -- the sidebar's "Artistes" total on libraries
                         -- with many featurings / collaborations.
                         COUNT(DISTINCT ta.artist_id)     AS artist_count
                    FROM track t
                    LEFT JOIN track_artist ta ON ta.track_id = t.id
                   WHERE t.is_available = 1
                   GROUP BY t.library_id
              ) tc ON tc.library_id = l.id
              LEFT JOIN (
                  SELECT t.library_id, COUNT(DISTINCT tg.genre_id) AS genre_count
                    FROM track t
                    JOIN track_genre tg ON tg.track_id = t.id
                   WHERE t.is_available = 1
                   GROUP BY t.library_id
              ) gc ON gc.library_id = l.id
              LEFT JOIN (
                  SELECT library_id, COUNT(*) AS folder_count
                    FROM library_folder
                   GROUP BY library_id
              ) f ON f.library_id = l.id
             ORDER BY l.updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(libraries)
    }

    async fn exists(&self, id: i64) -> CoreResult<bool> {
        let row: Option<i64> = sqlx::query_scalar("SELECT id FROM library WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    async fn insert(&self, draft: &LibraryDraft) -> CoreResult<i64> {
        let mut conn = self.pool.acquire().await?;
        insert_conn(&mut conn, draft).await
    }

    async fn update(&self, id: i64, patch: &LibraryUpdate, now_ms: i64) -> CoreResult<()> {
        let mut conn = self.pool.acquire().await?;
        // Drop the boolean for trait back-compat — the existing
        // callers don't read it. Tx-aware sites call `update_conn`
        // directly so they can collapse the existence-check into
        // the write.
        let _ = update_conn(&mut conn, id, patch, now_ms).await?;
        Ok(())
    }

    async fn delete(&self, id: i64) -> CoreResult<bool> {
        let mut conn = self.pool.acquire().await?;
        delete_conn(&mut conn, id).await
    }

    async fn touch_updated_at(&self, id: i64, now_ms: i64) -> CoreResult<()> {
        sqlx::query("UPDATE library SET updated_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_folders(&self, library_id: i64) -> CoreResult<Vec<LibraryFolder>> {
        let rows = sqlx::query_as::<_, LibraryFolder>(
            "SELECT id, library_id, path, last_scanned_at, is_watched
               FROM library_folder
              WHERE library_id = ?
              ORDER BY id",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    async fn list_folder_ids(&self, library_id: i64) -> CoreResult<Vec<i64>> {
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM library_folder WHERE library_id = ? ORDER BY id",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }

    async fn insert_folder(&self, library_id: i64, path: &str) -> CoreResult<i64> {
        let result = sqlx::query(
            "INSERT INTO library_folder (library_id, path, last_scanned_at, is_watched)
             VALUES (?, ?, NULL, 0)",
        )
        .bind(library_id)
        .bind(path)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    async fn insert_or_get_folder(&self, library_id: i64, path: &str) -> CoreResult<i64> {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO library_folder
                 (library_id, path, last_scanned_at, is_watched)
             VALUES (?, ?, NULL, 0)",
        )
        .bind(library_id)
        .bind(path)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() > 0 {
            return Ok(res.last_insert_rowid());
        }
        // INSERT OR IGNORE no-op means the row already existed —
        // resolve it by (library_id, path).
        let id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM library_folder WHERE library_id = ? AND path = ?",
        )
        .bind(library_id)
        .bind(path)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    async fn delete_folder_with_tracks(&self, folder_id: i64) -> CoreResult<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM track WHERE folder_id = ?")
            .bind(folder_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM library_folder WHERE id = ?")
            .bind(folder_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
