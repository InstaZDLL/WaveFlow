//! Canonical-fields builder + `(hlc, payload_hash)` stamp for the
//! `track_rating` entity.
//!
//! Server (`apply::rating` in waveflow-server `src/apply.rs`) keeps
//! `user_track_rating` as a sibling table keyed on
//! `(user_id, file_hash)` with a single synced field `rating: i64`
//! (0..=255 POPM range). On desktop the per-profile DB IS the user
//! scope, so the rating column rides on the same `track` row as the
//! metadata — under a `rating_` prefix on the HLC + payload_hash
//! columns so the rating's §2 tuple stays independent of the
//! metadata-edit tuple. Same physical row, two logical sync
//! entities; the `metadata_digest_version` table seeds both
//! `track` and `track_rating` as separate counters in A.3.
//!
//! Canonical form: `{"rating": <i64>}` for set ops, empty for delete
//! (rating cleared). The empty-canonical-on-delete shape matches
//! the server's "DELETE drops the row, no payload_hash needed"
//! behaviour — the bump still fires to invalidate the digest.

use serde_json::{Map, Value};
use sqlx::SqliteConnection;
use waveflow_core::sync::canon;

use crate::error::{AppError, AppResult};
use crate::sync::hooks::EnqueuedStamp;
use crate::sync::payload::{bump_digest_in_tx, payload_hash};

/// Build the canonical-fields map for a `track_rating` set op.
/// Single key `rating` carrying the POPM-range value the caller is
/// about to persist.
pub fn canonical_fields(rating: i64) -> Map<String, Value> {
    let mut m = Map::new();
    canon::i64(&mut m, "rating", rating);
    m
}

/// Stamp a `track_rating` SET op on the rating-prefixed HLC + hash
/// columns of `track.id = track_id`, and bump
/// `metadata_digest_version['track_rating']`. The regular
/// `hlc_*` + `payload_hash` columns owned by [`super::track`] are
/// untouched — they track the metadata sub-entity.
///
/// Must be called AFTER the `UPDATE track SET rating = ? WHERE id = ?`
/// has landed and AFTER [`crate::sync::hooks::enqueue_op_in_tx`]
/// returned `Some(stamp)`.
pub async fn stamp_set_in_tx(
    conn: &mut SqliteConnection,
    track_id: i64,
    rating: i64,
    stamp: EnqueuedStamp,
) -> AppResult<()> {
    let hash = payload_hash(&canonical_fields(rating), stamp);
    let res = sqlx::query(
        "UPDATE track
            SET rating_hlc_wall = ?,
                rating_hlc_logical = ?,
                rating_origin_device_id = ?,
                rating_payload_hash = ?
          WHERE id = ?",
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
            "sync::payload::track_rating::stamp_set_in_tx: no track row matched id {track_id}",
        )));
    }
    bump_digest_in_tx(conn, "track_rating").await?;
    Ok(())
}

/// Stamp a `track_rating` DELETE op on the rating-prefixed columns.
/// The track row still exists (the rating column itself just went
/// NULL); we clear the rating_payload_hash to mirror the server's
/// "DELETE drops the user_track_rating row" shape, but keep the
/// HLC + origin pair so a future LWW order check against an older
/// peer write can still resolve.
pub async fn stamp_delete_in_tx(
    conn: &mut SqliteConnection,
    track_id: i64,
    stamp: EnqueuedStamp,
) -> AppResult<()> {
    let res = sqlx::query(
        "UPDATE track
            SET rating_hlc_wall = ?,
                rating_hlc_logical = ?,
                rating_origin_device_id = ?,
                rating_payload_hash = NULL
          WHERE id = ?",
    )
    .bind(stamp.hlc_wall)
    .bind(stamp.hlc_logical)
    .bind(stamp.origin_device_id.map(|u| u.to_string()))
    .bind(track_id)
    .execute(&mut *conn)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!(
            "sync::payload::track_rating::stamp_delete_in_tx: no track row matched id {track_id}",
        )));
    }
    bump_digest_in_tx(conn, "track_rating").await?;
    Ok(())
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
            "CREATE TABLE track (
                id INTEGER PRIMARY KEY,
                rating INTEGER,
                rating_hlc_wall INTEGER NOT NULL DEFAULT 0,
                rating_hlc_logical INTEGER NOT NULL DEFAULT 0,
                rating_origin_device_id TEXT,
                rating_payload_hash BLOB
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
            "INSERT INTO metadata_digest_version (entity, version) VALUES ('track_rating', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[test]
    fn canonical_fields_has_single_rating_key() {
        let m = canonical_fields(180);
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("rating").unwrap(), &Value::from(180));
    }

    #[tokio::test]
    async fn stamp_set_in_tx_writes_hash_and_bumps_digest() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO track (id, rating) VALUES (7, 200)")
            .execute(&mut *conn)
            .await
            .unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1_700_000_000_001,
            hlc_logical: 4,
            origin_device_id: None,
        };
        stamp_set_in_tx(&mut conn, 7, 200, stamp).await.unwrap();

        let row: (i64, i32, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT rating_hlc_wall, rating_hlc_logical, rating_payload_hash FROM track WHERE id = 7",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(row.0, 1_700_000_000_001);
        assert_eq!(row.1, 4);
        assert_eq!(row.2.as_deref().map(|b| b.len()), Some(32));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'track_rating'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }

    #[tokio::test]
    async fn stamp_delete_in_tx_clears_hash_and_bumps_digest() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO track (id, rating, rating_payload_hash) VALUES (7, 200, X'00112233')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1_700_000_000_002,
            hlc_logical: 0,
            origin_device_id: None,
        };
        stamp_delete_in_tx(&mut conn, 7, stamp).await.unwrap();

        let row: (i64, Option<Vec<u8>>) =
            sqlx::query_as("SELECT rating_hlc_wall, rating_payload_hash FROM track WHERE id = 7")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(row.0, 1_700_000_000_002);
        assert!(row.1.is_none(), "rating_payload_hash cleared on delete");

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'track_rating'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }

    #[tokio::test]
    async fn stamp_set_in_tx_errors_when_row_missing() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1,
            hlc_logical: 0,
            origin_device_id: None,
        };
        let err = stamp_set_in_tx(&mut conn, 999, 50, stamp)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("no track row matched id 999"));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'track_rating'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 0);
    }

    #[tokio::test]
    async fn stamp_delete_in_tx_errors_when_row_missing() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1,
            hlc_logical: 0,
            origin_device_id: None,
        };
        let err = stamp_delete_in_tx(&mut conn, 999, stamp).await.unwrap_err();
        assert!(format!("{err}").contains("no track row matched id 999"));
    }
}
