//! Canonical-fields builder + `(hlc, payload_hash)` stamp for the
//! `liked_track` entity.
//!
//! `liked` is a binary state on the server (`apply::liked` in
//! waveflow-server `src/apply.rs`) — no synced fields beyond the
//! row identity itself, so the canonical form is just `{}` and the
//! `payload_hash` distinguishes rows purely by HLC + origin under
//! the §2 tuple.
//!
//! ## Key shape — wire vs row
//!
//! On the wire, `entity: "liked_track"` carries `entity_id =
//! track.file_hash` (the cross-device identity for tracks). On
//! desktop, `liked_track` is a sibling table keyed on `track_id`
//! (REFERENCES `track(id)`), so [`stamp_in_tx`] resolves through
//! the rowid; the caller already knows the track_id at the
//! enqueue site (`commands/track.rs::toggle_like_track`).

use serde_json::{Map, Value};
use sqlx::SqliteConnection;

use crate::error::{AppError, AppResult};
use crate::sync::hooks::EnqueuedStamp;
use crate::sync::payload::{bump_digest_in_tx, payload_hash};

/// Build the canonical-fields map for a `liked_track` row. Always
/// empty — see module docs.
pub fn canonical_fields() -> Map<String, Value> {
    Map::new()
}

/// Stamp the `liked_track` row for `track_id` with
/// `(hlc, origin_device_id, payload_hash)` and bump the local
/// `metadata_digest_version` counter for `liked_track`.
///
/// Must be called AFTER the `INSERT INTO liked_track (track_id, ...)`
/// (or `INSERT OR IGNORE`'s successful materialisation — caller
/// gates on `rows_affected() > 0` to skip phantom enqueues; see
/// `commands/track.rs::toggle_like_track`) and AFTER
/// [`crate::sync::hooks::enqueue_op_in_tx`] returned `Some(stamp)`.
pub async fn stamp_in_tx(
    conn: &mut SqliteConnection,
    track_id: i64,
    stamp: EnqueuedStamp,
) -> AppResult<()> {
    let hash = payload_hash(&canonical_fields(), stamp);
    let res = sqlx::query(
        "UPDATE liked_track
            SET hlc_wall = ?,
                hlc_logical = ?,
                origin_device_id = ?,
                payload_hash = ?
          WHERE track_id = ?",
    )
    .bind(stamp.hlc_wall)
    .bind(stamp.hlc_logical)
    .bind(stamp.origin_device_id.map(|u| u.to_string()))
    .bind(&hash[..])
    .bind(track_id)
    .execute(&mut *conn)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!(
            "sync::payload::liked_track::stamp_in_tx: no liked_track row matched track_id {track_id}",
        )));
    }
    bump_digest_in_tx(conn, "liked_track").await?;
    Ok(())
}

/// Bump-only path for `liked_track` DELETE ops (the user unliked
/// the track). The row is already gone post-DELETE — there's
/// nothing to UPDATE — but the digest still has to move so the
/// set member's removal is visible to the backfill protocol.
pub async fn bump_for_delete_in_tx(conn: &mut SqliteConnection) -> AppResult<()> {
    bump_digest_in_tx(conn, "liked_track").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE liked_track (
                track_id INTEGER PRIMARY KEY,
                liked_at INTEGER NOT NULL,
                hlc_wall INTEGER NOT NULL DEFAULT 0,
                hlc_logical INTEGER NOT NULL DEFAULT 0,
                origin_device_id TEXT,
                payload_hash BLOB
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE metadata_digest_version (
                entity TEXT PRIMARY KEY,
                version INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metadata_digest_version (entity, version) VALUES ('liked_track', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[test]
    fn canonical_fields_is_empty() {
        assert_eq!(canonical_fields().len(), 0);
    }

    #[tokio::test]
    async fn stamp_in_tx_writes_hash_and_bumps_digest() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO liked_track (track_id, liked_at) VALUES (42, 1000)")
            .execute(&mut *conn)
            .await
            .unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1_700_000_000_001,
            hlc_logical: 3,
            origin_device_id: None,
        };
        stamp_in_tx(&mut conn, 42, stamp).await.unwrap();

        let row: (i64, i32, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT hlc_wall, hlc_logical, payload_hash FROM liked_track WHERE track_id = 42",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(row.0, 1_700_000_000_001);
        assert_eq!(row.1, 3);
        assert_eq!(row.2.as_deref().map(|b| b.len()), Some(32));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'liked_track'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }

    #[tokio::test]
    async fn stamp_in_tx_errors_when_row_missing() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1,
            hlc_logical: 0,
            origin_device_id: None,
        };
        let err = stamp_in_tx(&mut conn, 999, stamp).await.unwrap_err();
        assert!(format!("{err}").contains("no liked_track row matched track_id 999"));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'liked_track'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 0);
    }

    #[tokio::test]
    async fn bump_for_delete_in_tx_increments_digest_only() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        bump_for_delete_in_tx(&mut conn).await.unwrap();
        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'liked_track'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }
}
