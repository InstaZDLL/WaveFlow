//! Canonical-fields builder + `(hlc, payload_hash)` stamp for the
//! `library` entity.
//!
//! The field set mirrors the server's
//! `apply::library::canonical_fields` (waveflow-server `src/apply.rs`)
//! exactly — same key names, same `Value::Null` shape on `None`, so
//! a desktop INSERT and a server apply on the same logical state
//! land identical bytes through `compute_payload_hash`. The Phase B
//! backfill protocol diffs `(canonical_id, payload_hash)` pairs
//! between the two replicas; any divergence here would surface as
//! a false-positive "needs re-sync" for every library row.

use serde_json::{Map, Value};
use sqlx::SqliteConnection;
use waveflow_core::sync::canon;

use crate::error::AppResult;
use crate::sync::hooks::EnqueuedStamp;
use crate::sync::payload::{bump_digest_in_tx, payload_hash};

/// Build the canonical-fields map for a `library` row. Caller passes
/// the four synced scalars verbatim — the helper handles the
/// `Value::Null` vs `Value::String` shape so the `Map<String, Value>`
/// matches the server byte-for-byte.
pub fn canonical_fields(
    name: &str,
    description: Option<&str>,
    color_id: &str,
    icon_id: &str,
) -> Map<String, Value> {
    let mut m = Map::new();
    canon::string(&mut m, "name", name);
    canon::opt_string(&mut m, "description", description);
    canon::string(&mut m, "color_id", color_id);
    canon::string(&mut m, "icon_id", icon_id);
    m
}

/// Stamp an INSERT-or-UPDATE on the `library` row identified by
/// `local_id` with `(hlc, origin_device_id, payload_hash)` and bump
/// the local `metadata_digest_version` counter — all in the
/// caller's open transaction.
///
/// Must be called AFTER the row's data has landed (so the canonical
/// fields the caller hands in match what's actually persisted) and
/// AFTER [`crate::sync::hooks::enqueue_op_in_tx`] returned
/// `Some(stamp)` (the HLC pair in the stamp must match the one the
/// queue row carries on the wire so a peer that pulls the op
/// computes the same `payload_hash`).
pub async fn stamp_in_tx(
    conn: &mut SqliteConnection,
    local_id: i64,
    fields: Map<String, Value>,
    stamp: EnqueuedStamp,
) -> AppResult<()> {
    let hash = payload_hash(&fields, stamp);
    sqlx::query(
        "UPDATE library
            SET hlc_wall = ?,
                hlc_logical = ?,
                origin_device_id = ?,
                payload_hash = ?
          WHERE id = ?",
    )
    .bind(stamp.hlc_wall)
    .bind(stamp.hlc_logical)
    .bind(stamp.origin_device_id.map(|u| u.to_string()))
    .bind(&hash[..])
    .bind(local_id)
    .execute(&mut *conn)
    .await?;
    bump_digest_in_tx(conn, "library").await?;
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
        // Minimal library + digest_version schema; mirrors the
        // post-A.3 migration columns the stamp touches.
        sqlx::query(
            "CREATE TABLE library (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                color_id TEXT NOT NULL DEFAULT 'emerald',
                icon_id TEXT NOT NULL DEFAULT 'library',
                created_at INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0,
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
        sqlx::query("INSERT INTO metadata_digest_version (entity, version) VALUES ('library', 0)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[test]
    fn canonical_fields_has_four_keys_with_explicit_null_on_none() {
        let m = canonical_fields("Mix", None, "violet", "music");
        assert_eq!(m.len(), 4);
        assert_eq!(m.get("name").unwrap(), &Value::String("Mix".into()));
        assert_eq!(m.get("description").unwrap(), &Value::Null);
        assert_eq!(m.get("color_id").unwrap(), &Value::String("violet".into()));
        assert_eq!(m.get("icon_id").unwrap(), &Value::String("music".into()));
    }

    #[test]
    fn canonical_fields_matches_server_shape() {
        // Smoke test that the desktop's canonical-fields keys are
        // exactly the four the server's apply::library expects.
        // A drift here is what the cross-repo backfill protocol
        // would silently catch as "every library row diverges".
        let m = canonical_fields("Mix", Some("d"), "violet", "music");
        let keys: Vec<&String> = m.keys().collect();
        assert!(keys.iter().any(|k| k.as_str() == "name"));
        assert!(keys.iter().any(|k| k.as_str() == "description"));
        assert!(keys.iter().any(|k| k.as_str() == "color_id"));
        assert!(keys.iter().any(|k| k.as_str() == "icon_id"));
    }

    #[tokio::test]
    async fn stamp_in_tx_writes_hash_and_bumps_digest() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO library (id, name) VALUES (1, 'Mix')")
            .execute(&mut *conn)
            .await
            .unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1_700_000_000_001,
            hlc_logical: 7,
            origin_device_id: None,
        };
        let fields = canonical_fields("Mix", None, "emerald", "library");
        stamp_in_tx(&mut conn, 1, fields, stamp).await.unwrap();

        let row: (i64, i32, Option<String>, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT hlc_wall, hlc_logical, origin_device_id, payload_hash FROM library WHERE id = 1",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(row.0, 1_700_000_000_001);
        assert_eq!(row.1, 7);
        assert_eq!(row.2, None);
        assert_eq!(row.3.as_deref().map(|b| b.len()), Some(32));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'library'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }

    #[tokio::test]
    async fn stamp_in_tx_re_emit_produces_identical_hash_with_same_fields() {
        // Re-stamping the same row with the same fields + the same
        // HLC must produce the same hash (idempotent). The HLC
        // differs in practice (each enqueue draws a new one), but
        // the hash must be a pure function of (fields, hlc, origin).
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO library (id, name) VALUES (1, 'Mix')")
            .execute(&mut *conn)
            .await
            .unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1,
            hlc_logical: 0,
            origin_device_id: None,
        };
        let fields = canonical_fields("Mix", None, "emerald", "library");
        stamp_in_tx(&mut conn, 1, fields.clone(), stamp)
            .await
            .unwrap();
        let h1: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT payload_hash FROM library WHERE id = 1")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        stamp_in_tx(&mut conn, 1, fields, stamp).await.unwrap();
        let h2: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT payload_hash FROM library WHERE id = 1")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(h1, h2);
    }
}
