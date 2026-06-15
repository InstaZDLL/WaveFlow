//! Phase B.0 — per-entity `payload_hash` stamping + local
//! `metadata_digest_version` bump.
//!
//! Every CRUD path that emits a sync op via
//! [`crate::sync::hooks::enqueue_op_in_tx`] also has to:
//!
//! 1. Compute the row's `payload_hash` over its canonical wire form
//!    (RFC-003 §4) using the same HLC + origin_device_id the queue
//!    row was just stamped with.
//! 2. Write `(hlc_wall, hlc_logical, origin_device_id, payload_hash)`
//!    onto the entity row inside the same SQLite transaction.
//! 3. Bump the `metadata_digest_version` counter for that entity in
//!    the same tx so a digest endpoint hit afterwards sees the new
//!    state via the cache-invalidation invariant
//!    (see `waveflow-server` apply::*::… for the mirror invariant).
//!
//! Without (1)+(2), the desktop can't participate in the Phase B
//! backfill — the row's `payload_hash` stays NULL and the digest
//! comparison would always pick this row as "missing on the
//! desktop" even when the bytes match the server's view.
//!
//! Each entity gets its own submodule so the canonical-fields shape
//! lives next to the row layout in `commands/<entity>.rs`. The
//! shared bit ([`bump_digest_in_tx`]) is generic over the entity
//! name; the table-specific row UPDATE is per-entity because the
//! column / id shape differs.

use sqlx::SqliteConnection;
use waveflow_core::sync::Hlc;
use waveflow_core::sync::payload_hash::compute_payload_hash;

use crate::error::AppResult;
use crate::sync::hooks::EnqueuedStamp;

pub mod library;
pub mod liked_track;
pub mod playlist;
pub mod track;
pub mod track_rating;

/// Bump the per-entity counter that `waveflow-server`'s digest
/// endpoint reads to invalidate its cache (RFC-003 §4 — "every
/// apply handler that mutates a row's payload_hash MUST atomically
/// bump the counter in the same transaction"). Desktop-side bump
/// mirrors the server's bump so the local digest snapshot the
/// backfill protocol computes shares the same monotone source of
/// truth.
///
/// The counter table was seeded in the A.3 migration with one row
/// per profile-scoped entity (`library`, `track`, `playlist`,
/// `playlist_track`, `liked_track`, `track_rating`). An UPSERT keeps
/// the helper robust against a schema-evolution gap where a future
/// entity name lands before its seed migration does.
pub async fn bump_digest_in_tx(conn: &mut SqliteConnection, entity: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO metadata_digest_version (entity, version) VALUES (?, 1)
         ON CONFLICT(entity) DO UPDATE
            SET version = metadata_digest_version.version + 1",
    )
    .bind(entity)
    .execute(conn)
    .await?;
    Ok(())
}

/// Compute the BLAKE3-256 `payload_hash` for an entity row given its
/// canonical fields + the HLC + origin_device_id [`EnqueuedStamp`]
/// returned by [`crate::sync::hooks::enqueue_op_in_tx`]. Wraps the
/// `waveflow-core` helper so a divergent server-side build can't
/// cause the desktop to compute a structurally different hash —
/// they share a single canonical-serialise + BLAKE3 implementation.
pub fn payload_hash(
    fields: &serde_json::Map<String, serde_json::Value>,
    stamp: EnqueuedStamp,
) -> [u8; 32] {
    let hlc = Hlc {
        wall: stamp.hlc_wall,
        logical: stamp.hlc_logical,
    };
    compute_payload_hash(fields, hlc, stamp.origin_device_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> sqlx::SqlitePool {
        // max_connections=1 + a `:memory:` pool means tests must not
        // re-acquire while holding a conn — the second acquire
        // blocks until `PoolTimedOut`. The tests below drop the
        // acquired conn before any pool-level SELECT for that
        // reason.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
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
        pool
    }

    #[tokio::test]
    async fn bump_increments_existing_seeded_row() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO metadata_digest_version (entity, version) VALUES ('library', 5)")
            .execute(&mut *conn)
            .await
            .unwrap();
        bump_digest_in_tx(&mut conn, "library").await.unwrap();
        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'library'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 6);
    }

    #[tokio::test]
    async fn bump_inserts_row_for_unseeded_entity() {
        // Defence-in-depth: a future entity name landing before its
        // seed migration shouldn't crash on a `NULL` UPDATE — the
        // UPSERT plants version=1.
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        bump_digest_in_tx(&mut conn, "future_entity").await.unwrap();
        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'future_entity'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }

    #[tokio::test]
    async fn bump_is_monotonic_across_calls() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        bump_digest_in_tx(&mut conn, "library").await.unwrap();
        bump_digest_in_tx(&mut conn, "library").await.unwrap();
        bump_digest_in_tx(&mut conn, "library").await.unwrap();
        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'library'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 3);
    }

    #[test]
    fn payload_hash_is_deterministic_for_same_input() {
        use serde_json::{Map, Value};
        let mut fields: Map<String, Value> = Map::new();
        fields.insert("name".into(), Value::String("Mix".into()));
        let stamp = EnqueuedStamp {
            hlc_wall: 1_700_000_000_001,
            hlc_logical: 7,
            origin_device_id: None,
        };
        let h1 = payload_hash(&fields, stamp);
        let h2 = payload_hash(&fields, stamp);
        assert_eq!(h1, h2);
    }

    #[test]
    fn payload_hash_changes_with_hlc() {
        use serde_json::{Map, Value};
        let mut fields: Map<String, Value> = Map::new();
        fields.insert("name".into(), Value::String("Mix".into()));
        let h1 = payload_hash(
            &fields,
            EnqueuedStamp {
                hlc_wall: 1,
                hlc_logical: 0,
                origin_device_id: None,
            },
        );
        let h2 = payload_hash(
            &fields,
            EnqueuedStamp {
                hlc_wall: 2,
                hlc_logical: 0,
                origin_device_id: None,
            },
        );
        assert_ne!(h1, h2);
    }
}
