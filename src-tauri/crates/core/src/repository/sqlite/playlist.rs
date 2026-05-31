//! SQLite implementation of [`PlaylistRepository`].
//!
//! Public write helpers come in two flavours:
//!
//! - **Trait methods** on [`SqlitePlaylistRepository`] — take `&self`
//!   and run against the struct's owned `pool`. Backwards-compatible
//!   for every existing caller.
//! - **Free `*_conn` functions** at the module root — take
//!   `&mut SqliteConnection` so the caller can compose the playlist
//!   write inside their own transaction (e.g. the desktop's
//!   write-then-enqueue path that 1.f.desktop.2b would otherwise
//!   leave non-atomic). The trait methods delegate to these.

use async_trait::async_trait;
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use crate::{
    domain::playlist::Playlist,
    error::CoreResult,
    repository::playlist::{PlaylistDraft, PlaylistRepository, PlaylistUpdate},
};

// ── Connection-taking helpers ───────────────────────────────────
//
// Each function does the same write the matching trait method did,
// but against a caller-supplied connection. The trait methods below
// just acquire `pool.begin()` and forward; new tx-aware call sites
// (e.g. `commands/playlist.rs` post Phase 1.f.desktop.4) call these
// directly so the playlist write + outbox enqueue land in one atomic
// commit.

/// Insert a custom (`is_smart = 0`) playlist with `position = 0`.
/// Returns the assigned rowid.
pub async fn insert_custom_conn(
    conn: &mut SqliteConnection,
    draft: &PlaylistDraft,
) -> CoreResult<i64> {
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
    .execute(conn)
    .await?;
    Ok(insert.last_insert_rowid())
}

/// Partial `UPDATE playlist` via `COALESCE` — every `Some` field
/// overwrites, every `None` leaves the existing value alone.
///
/// Returns `true` when a row was actually touched, `false` when no
/// playlist matched. The boolean lets the desktop's tx-aware caller
/// drop the surrounding transaction without enqueueing outbox ops
/// against a row that vanished — same-tx existence check + write +
/// outbox in a single atomic pass.
pub async fn update_conn(
    conn: &mut SqliteConnection,
    id: i64,
    patch: &PlaylistUpdate,
    now_ms: i64,
) -> CoreResult<bool> {
    let res = sqlx::query(
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
    .execute(conn)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// `true` when a row was deleted, `false` when no row matched.
pub async fn delete_conn(conn: &mut SqliteConnection, id: i64) -> CoreResult<bool> {
    let result = sqlx::query("DELETE FROM playlist WHERE id = ?")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Append a single track at the tail. Same `(insert + bump
/// playlist.updated_at)` shape the pool variant runs, just inside the
/// caller's open connection so it composes with other writes.
pub async fn append_track_conn(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    track_id: i64,
    now_ms: i64,
) -> CoreResult<()> {
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
    .execute(&mut *conn)
    .await?;

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(playlist_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// Bulk append. Returns the count of rows that were actually
/// inserted (duplicates skipped by `INSERT OR IGNORE` don't count).
pub async fn append_tracks_conn(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    track_ids: &[i64],
    now_ms: i64,
) -> CoreResult<u32> {
    if track_ids.is_empty() {
        return Ok(0);
    }
    let current_max: Option<i64> =
        sqlx::query_scalar("SELECT MAX(position) FROM playlist_track WHERE playlist_id = ?")
            .bind(playlist_id)
            .fetch_one(&mut *conn)
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
        .execute(&mut *conn)
        .await?;
        if result.rows_affected() > 0 {
            inserted += 1;
            next_position += 1;
        }
    }

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(playlist_id)
        .execute(conn)
        .await?;
    Ok(inserted)
}

/// Remove a track and renumber the tail so positions stay contiguous.
/// `true` on success, `false` when the track wasn't in the playlist.
pub async fn remove_track_conn(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    track_id: i64,
    now_ms: i64,
) -> CoreResult<bool> {
    let removed_position: Option<i64> = sqlx::query_scalar(
        "SELECT position FROM playlist_track WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist_id)
    .bind(track_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(pos) = removed_position else {
        return Ok(false);
    };

    sqlx::query("DELETE FROM playlist_track WHERE playlist_id = ? AND track_id = ?")
        .bind(playlist_id)
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "UPDATE playlist_track
            SET position = position - 1
          WHERE playlist_id = ? AND position > ?",
    )
    .bind(playlist_id)
    .bind(pos)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(playlist_id)
        .execute(conn)
        .await?;
    Ok(true)
}

/// Move a track to `new_position`. Returns the effective position
/// after the repo's internal clamp (`new_position.clamp(0, len - 1)`),
/// or `None` when the track wasn't in the playlist. Lets the caller
/// stamp the sync payload with the row's actual new state — see the
/// commentary in `commands/playlist.rs::reorder_playlist_track` for
/// why this matters.
pub async fn reorder_track_conn(
    conn: &mut SqliteConnection,
    playlist_id: i64,
    track_id: i64,
    new_position: i64,
    now_ms: i64,
) -> CoreResult<Option<i64>> {
    let from: Option<i64> = sqlx::query_scalar(
        "SELECT position FROM playlist_track WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(playlist_id)
    .bind(track_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(from) = from else {
        return Ok(None);
    };

    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist_track WHERE playlist_id = ?")
        .bind(playlist_id)
        .fetch_one(&mut *conn)
        .await?;
    let to = new_position.clamp(0, (len - 1).max(0));

    if from == to {
        return Ok(Some(to));
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
        .execute(&mut *conn)
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
        .execute(&mut *conn)
        .await?;
    }

    sqlx::query(
        "UPDATE playlist_track SET position = ?
          WHERE playlist_id = ? AND track_id = ?",
    )
    .bind(to)
    .bind(playlist_id)
    .bind(track_id)
    .execute(&mut *conn)
    .await?;

    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now_ms)
        .bind(playlist_id)
        .execute(conn)
        .await?;
    Ok(Some(to))
}

/// Begin a transaction on the supplied pool — exposed so the desktop
/// command sites can compose playlist writes + outbox enqueues in a
/// single atomic commit without owning the repo struct.
pub async fn begin_tx(pool: &SqlitePool) -> CoreResult<Transaction<'_, Sqlite>> {
    Ok(pool.begin().await?)
}

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
        let mut conn = self.pool.acquire().await?;
        insert_custom_conn(&mut conn, draft).await
    }

    async fn update(&self, id: i64, patch: &PlaylistUpdate, now_ms: i64) -> CoreResult<()> {
        let mut conn = self.pool.acquire().await?;
        // Drop the boolean for trait back-compat — the trait method
        // never communicated existence to the caller and changing it
        // would break every non-tx-aware site (smart playlists,
        // playlist_cover regen, etc.). Sites that need the signal
        // call `update_conn` directly.
        let _ = update_conn(&mut conn, id, patch, now_ms).await?;
        Ok(())
    }

    async fn delete(&self, id: i64) -> CoreResult<bool> {
        let mut conn = self.pool.acquire().await?;
        delete_conn(&mut conn, id).await
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
        // Wrap in a tx so the `INSERT OR IGNORE` + the
        // `UPDATE playlist.updated_at` land atomically — without one,
        // a concurrent reader could see the track added but the
        // playlist row still stamped at the pre-add `updated_at`.
        let mut tx = self.pool.begin().await?;
        append_track_conn(&mut tx, playlist_id, track_id, now_ms).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn append_tracks(
        &self,
        playlist_id: i64,
        track_ids: &[i64],
        now_ms: i64,
    ) -> CoreResult<u32> {
        let mut tx = self.pool.begin().await?;
        let inserted = append_tracks_conn(&mut tx, playlist_id, track_ids, now_ms).await?;
        tx.commit().await?;
        Ok(inserted)
    }

    async fn remove_track(&self, playlist_id: i64, track_id: i64, now_ms: i64) -> CoreResult<bool> {
        let mut tx = self.pool.begin().await?;
        let removed = remove_track_conn(&mut tx, playlist_id, track_id, now_ms).await?;
        tx.commit().await?;
        Ok(removed)
    }

    async fn reorder_track(
        &self,
        playlist_id: i64,
        track_id: i64,
        new_position: i64,
        now_ms: i64,
    ) -> CoreResult<bool> {
        let mut tx = self.pool.begin().await?;
        let effective = reorder_track_conn(&mut tx, playlist_id, track_id, new_position, now_ms)
            .await?;
        tx.commit().await?;
        Ok(effective.is_some())
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
