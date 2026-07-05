//! Canonical-fields builder + `(hlc, payload_hash)` stamp for the
//! `playlist` entity.
//!
//! The field set mirrors the server's
//! `apply::playlist::canonical_fields` (waveflow-server `src/apply.rs`)
//! exactly — same key names, same `Value::Null` shape on `None`. The
//! desktop CRUD path and the server apply pipeline land identical
//! bytes through `compute_payload_hash` for the same logical state,
//! so the Phase B backfill diff sees zero spurious "needs re-sync"
//! rows. Sibling of [`super::library`] — same 4-key shape (name +
//! opt description + color_id + icon_id), keyed on the playlist's
//! local rowid.

use serde_json::{Map, Value};
use sqlx::SqliteConnection;
use waveflow_core::sync::canon;

use crate::error::{AppError, AppResult};
use crate::sync::hooks::EnqueuedStamp;
use crate::sync::payload::{bump_digest_in_tx, payload_hash};

/// Build the canonical-fields map for a `playlist` row. Caller passes
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

/// Stamp an INSERT-or-UPDATE on the `playlist` row identified by
/// `local_id` with `(hlc, origin_device_id, payload_hash)` and bump
/// the local `metadata_digest_version` counter — all in the
/// caller's open transaction.
///
/// Must be called AFTER the row's data has landed and AFTER
/// [`crate::sync::hooks::enqueue_op_in_tx`] returned `Some(stamp)`
/// (the HLC pair in the stamp must match the one the queue row
/// carries on the wire so a peer computes the same `payload_hash`).
pub async fn stamp_in_tx(
    conn: &mut SqliteConnection,
    local_id: i64,
    fields: Map<String, Value>,
    stamp: EnqueuedStamp,
) -> AppResult<()> {
    let hash = payload_hash(&fields, stamp);
    let res = sqlx::query(
        "UPDATE playlist
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
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!(
            "sync::payload::playlist::stamp_in_tx: no playlist row matched id {local_id}",
        )));
    }
    bump_digest_in_tx(conn, "playlist").await?;
    Ok(())
}

/// Read back the canonical fields of a `playlist` row via the
/// caller's open transaction. Defence-in-depth — if the CRUD path
/// ever gains trim / lowercase / default-substitution normalisation,
/// the desktop's `payload_hash` would silently diverge from what the
/// server computes on the same op's payload. Reading back from the
/// persisted row keeps the contract robust against that drift.
///
/// Returns `None` when the row is gone (race with a concurrent
/// delete, identical handling to the 0-row UPDATE branch above).
pub async fn fields_from_row(
    conn: &mut SqliteConnection,
    local_id: i64,
) -> AppResult<Option<Map<String, Value>>> {
    let row: Option<(String, Option<String>, String, String)> =
        sqlx::query_as("SELECT name, description, color_id, icon_id FROM playlist WHERE id = ?")
            .bind(local_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.map(|(name, description, color_id, icon_id)| {
        canonical_fields(&name, description.as_deref(), &color_id, &icon_id)
    }))
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
            "CREATE TABLE playlist (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                color_id TEXT NOT NULL DEFAULT 'emerald',
                icon_id TEXT NOT NULL DEFAULT 'music',
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
        sqlx::query("INSERT INTO metadata_digest_version (entity, version) VALUES ('playlist', 0)")
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
        sqlx::query("INSERT INTO playlist (id, name) VALUES (1, 'Mix')")
            .execute(&mut *conn)
            .await
            .unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1_700_000_000_001,
            hlc_logical: 7,
            origin_device_id: None,
        };
        let fields = canonical_fields("Mix", None, "emerald", "music");
        stamp_in_tx(&mut conn, 1, fields, stamp).await.unwrap();

        let row: (i64, i32, Option<String>, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT hlc_wall, hlc_logical, origin_device_id, payload_hash FROM playlist WHERE id = 1",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(row.0, 1_700_000_000_001);
        assert_eq!(row.1, 7);
        assert_eq!(row.2, None);
        assert_eq!(row.3.as_deref().map(|b| b.len()), Some(32));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'playlist'",
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
        let fields = canonical_fields("Ghost", None, "emerald", "music");
        let err = stamp_in_tx(&mut conn, 999, fields, stamp)
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("no playlist row matched id 999"));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'playlist'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 0);
    }

    #[tokio::test]
    async fn fields_from_row_returns_none_for_missing_row() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        assert!(fields_from_row(&mut conn, 42).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn fields_from_row_reads_persisted_state() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO playlist (id, name, description, color_id, icon_id)
             VALUES (1, 'Mix', 'desc', 'violet', 'music')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        let fields = fields_from_row(&mut conn, 1).await.unwrap().unwrap();
        assert_eq!(
            fields,
            canonical_fields("Mix", Some("desc"), "violet", "music")
        );
    }
}
