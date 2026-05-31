// `enqueue`, `list_pending`, `drop_acked`, `mark_failed` are the
// drain-task surface (1.f.desktop.4). Diagnostic commands here use
// only `count_pending` + `clear`.
#![allow(dead_code)]

//! Local pending-ops queue. Rows here are ops the user produced
//! while signed into a `waveflow-server`; a future drain task
//! (1.f.desktop.4) posts them to `/api/v1/sync/ops` and removes the
//! rows the server accepts.
//!
//! The queue is intentionally dumb storage — every write is a single
//! INSERT, every read a single SELECT. The orchestration (when to
//! drain, what to retry, how to back off) belongs to the drain task,
//! not the queue.
//!
//! ## Open design question — canonical entity ids
//!
//! Today the desktop stores `entity_id` as the TEXT-coerced local
//! `i64` from the SQLite tables (`playlist.id`, `library.id`, …).
//! This is fine for ops produced by THIS device — the server's
//! UNIQUE keys on `(user_id, device_id, entity, entity_id)` will
//! pick the right row.
//!
//! It does NOT cross devices cleanly: device A's playlist#42 and
//! device B's playlist#42 are not the same playlist. A future PR
//! introduces a stable `canonical_id UUID` on every syncable entity
//! and routes ops through that instead. Until then, the WS subscriber
//! has to translate incoming `entity_id`s through a `local_id ↔
//! canonical_id` mapping table that 1.f.desktop.4 introduces.
//!
//! Documenting the gap here so a reader of `enqueue` doesn't assume
//! the current shape is the final wire format.

use chrono::Utc;
use serde::Serialize;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use uuid::Uuid;

use crate::error::AppResult;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Draft of an op the caller wants to enqueue. The CRUD command site
/// builds this from its own context and hands it off to [`enqueue`];
/// the queue assigns the `operation_id`, the `id`, and the
/// `created_at` itself so nothing leaks across layers.
#[derive(Debug, Clone)]
pub struct PendingOpDraft {
    /// `"playlist" | "library" | "track" | …`. Free-form so the
    /// protocol can grow without a desktop release.
    pub entity: String,
    /// String-coerced entity id (see the module docstring on the
    /// canonical-id question).
    pub entity_id: String,
    /// `None` for whole-entity ops (insert / delete), `Some` for
    /// partial updates ("set name", "set color").
    pub field: Option<String>,
    /// `"set" | "delete" | "insert" | "noop"` — must match the
    /// migration's CHECK constraint.
    pub op: String,
    /// Op-specific JSON payload. Serialised to text at the SQL layer.
    pub payload: Option<serde_json::Value>,
}

/// A row read back from the queue. The drain task hydrates this and
/// hands it to the server; `id` is the local SQLite rowid the drain
/// later passes to [`drop_acked`].
#[derive(Debug, Clone, Serialize)]
pub struct PendingOp {
    pub id: i64,
    pub operation_id: String,
    pub lamport_ts: i64,
    pub entity: String,
    pub entity_id: String,
    pub field: Option<String>,
    pub op: String,
    pub payload: Option<serde_json::Value>,
    pub created_at: i64,
    pub last_attempt_at: Option<i64>,
    pub attempt_count: i64,
    pub last_error: Option<String>,
}

/// Append a draft to the queue under the caller-supplied
/// `lamport_ts`. The caller is responsible for drawing the lamport
/// value via [`crate::sync::lamport::next`] — splitting the two
/// responsibilities means the queue stays a pure-storage layer.
///
/// Returns the assigned `(id, operation_id)` so the caller can
/// correlate logs without an extra SELECT.
pub async fn enqueue(
    profile_pool: &SqlitePool,
    draft: &PendingOpDraft,
    lamport_ts: i64,
) -> AppResult<(i64, String)> {
    let operation_id = Uuid::new_v4().to_string();
    let payload_json = match draft.payload.as_ref() {
        Some(v) => Some(serde_json::to_string(v).map_err(|e| {
            crate::error::AppError::Other(format!("sync queue payload serialise: {e}"))
        })?),
        None => None,
    };
    let now = now_ms();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO sync_pending_op
            (operation_id, lamport_ts, entity, entity_id, field, op, payload, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(&operation_id)
    .bind(lamport_ts)
    .bind(&draft.entity)
    .bind(&draft.entity_id)
    .bind(draft.field.as_deref())
    .bind(&draft.op)
    .bind(payload_json.as_deref())
    .bind(now)
    .fetch_one(profile_pool)
    .await?;
    Ok((row.0, operation_id))
}

/// Raw row shape from `sync_pending_op`. Hydrated into [`PendingOp`]
/// (which carries the parsed JSON payload) by [`list_pending`]. Kept
/// private so callers see the cleaner public type.
#[derive(sqlx::FromRow)]
struct PendingOpRow {
    id: i64,
    operation_id: String,
    lamport_ts: i64,
    entity: String,
    entity_id: String,
    field: Option<String>,
    op: String,
    payload: Option<String>,
    created_at: i64,
    last_attempt_at: Option<i64>,
    attempt_count: i64,
    last_error: Option<String>,
}

/// Return the next `limit` pending rows in FIFO order. The drain
/// task calls this in a loop with a small `limit` so a flaky server
/// doesn't pin the whole queue in memory.
pub async fn list_pending(profile_pool: &SqlitePool, limit: i64) -> AppResult<Vec<PendingOp>> {
    let rows: Vec<PendingOpRow> = sqlx::query_as(
        "SELECT id, operation_id, lamport_ts, entity, entity_id, field, op,
                payload, created_at, last_attempt_at, attempt_count, last_error
         FROM sync_pending_op
         ORDER BY id ASC
         LIMIT ?",
    )
    .bind(limit)
    .fetch_all(profile_pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let payload = match row.payload {
            Some(text) => Some(serde_json::from_str(&text).map_err(|e| {
                crate::error::AppError::Other(format!(
                    "sync queue row {} payload parse: {e}",
                    row.id,
                ))
            })?),
            None => None,
        };
        out.push(PendingOp {
            id: row.id,
            operation_id: row.operation_id,
            lamport_ts: row.lamport_ts,
            entity: row.entity,
            entity_id: row.entity_id,
            field: row.field,
            op: row.op,
            payload,
            created_at: row.created_at,
            last_attempt_at: row.last_attempt_at,
            attempt_count: row.attempt_count,
            last_error: row.last_error,
        });
    }
    Ok(out)
}

/// Drop every row the server has acknowledged. The drain task
/// batches the ack lookup so we don't hammer the DB with one DELETE
/// per row.
///
/// Uses `QueryBuilder::push_bind` to construct the `IN (…)` list —
/// sqlx 0.9 rejects dynamically-built `&str` SQL on its `query()`
/// path (`SqlSafeStr` is only impl'd for `&'static str`), so the
/// typed builder is the canonical escape hatch.
pub async fn drop_acked(profile_pool: &SqlitePool, ids: &[i64]) -> AppResult<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut qb: QueryBuilder<Sqlite> =
        QueryBuilder::new("DELETE FROM sync_pending_op WHERE id IN (");
    let mut sep = qb.separated(", ");
    for id in ids {
        sep.push_bind(id);
    }
    sep.push_unseparated(")");
    let res = qb.build().execute(profile_pool).await?;
    Ok(res.rows_affected())
}

/// Mark a row as having failed its latest attempt, increment the
/// counter, and store the server's error string. Leaves the row
/// in the queue so the next drain pass can retry.
pub async fn mark_failed(profile_pool: &SqlitePool, id: i64, error: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE sync_pending_op
            SET last_attempt_at = ?,
                attempt_count = attempt_count + 1,
                last_error = ?
         WHERE id = ?",
    )
    .bind(now_ms())
    .bind(error)
    .bind(id)
    .execute(profile_pool)
    .await?;
    Ok(())
}

/// Number of rows currently waiting. Diagnostic only — the drain
/// task uses [`list_pending`] which already gives it a count.
pub async fn count_pending(profile_pool: &SqlitePool) -> AppResult<i64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_pending_op")
        .fetch_one(profile_pool)
        .await?;
    Ok(count)
}

/// Nuclear option for the Settings → Diagnostics panel. Drops every
/// row regardless of state. Returned for a confirmation toast.
pub async fn clear(profile_pool: &SqlitePool) -> AppResult<u64> {
    let res = sqlx::query("DELETE FROM sync_pending_op")
        .execute(profile_pool)
        .await?;
    Ok(res.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// Stand up an in-memory pool with the `sync_pending_op` schema
    /// copy-pasted from the migration. Updating the migration without
    /// updating this fixture would silently diverge the unit tests
    /// from production behaviour — a small price for not pulling the
    /// real migrator into the unit suite.
    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE sync_pending_op (
                id INTEGER PRIMARY KEY,
                operation_id TEXT NOT NULL UNIQUE,
                lamport_ts INTEGER NOT NULL UNIQUE CHECK (lamport_ts > 0),
                entity TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                field TEXT,
                op TEXT NOT NULL CHECK (op IN ('set','delete','insert','noop')),
                payload TEXT,
                created_at INTEGER NOT NULL,
                last_attempt_at INTEGER,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn draft(entity_id: &str, field: Option<&str>) -> PendingOpDraft {
        PendingOpDraft {
            entity: "playlist".into(),
            entity_id: entity_id.into(),
            field: field.map(str::to_string),
            op: "set".into(),
            payload: Some(serde_json::json!({ "value": "x" })),
        }
    }

    #[tokio::test]
    async fn enqueue_writes_row_and_returns_ids() {
        let pool = pool().await;
        let (id, operation_id) = enqueue(&pool, &draft("1", Some("name")), 1).await.unwrap();
        assert!(id > 0);
        Uuid::parse_str(&operation_id).expect("operation_id is a UUID");

        let rows = list_pending(&pool, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].operation_id, operation_id);
        assert_eq!(rows[0].lamport_ts, 1);
        assert_eq!(rows[0].entity, "playlist");
        assert_eq!(rows[0].entity_id, "1");
        assert_eq!(rows[0].field.as_deref(), Some("name"));
        assert_eq!(rows[0].op, "set");
        assert_eq!(
            rows[0].payload.as_ref().unwrap(),
            &serde_json::json!({ "value": "x" }),
        );
        assert_eq!(rows[0].attempt_count, 0);
        assert!(rows[0].last_attempt_at.is_none());
    }

    #[tokio::test]
    async fn list_pending_returns_fifo_order() {
        let pool = pool().await;
        for lamport in 1..=5 {
            enqueue(&pool, &draft(&lamport.to_string(), None), lamport)
                .await
                .unwrap();
        }
        let rows = list_pending(&pool, 10).await.unwrap();
        let lamports: Vec<i64> = rows.iter().map(|r| r.lamport_ts).collect();
        assert_eq!(lamports, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn lamport_unique_constraint_rejects_duplicates() {
        let pool = pool().await;
        enqueue(&pool, &draft("1", None), 42).await.unwrap();
        let err = enqueue(&pool, &draft("2", None), 42).await.unwrap_err();
        let s = format!("{err}");
        assert!(
            s.contains("UNIQUE") || s.contains("constraint"),
            "expected UNIQUE violation, got {s}",
        );
    }

    #[tokio::test]
    async fn drop_acked_removes_matching_rows() {
        let pool = pool().await;
        let mut ids = Vec::new();
        for lamport in 1..=5 {
            let (id, _) = enqueue(&pool, &draft(&lamport.to_string(), None), lamport)
                .await
                .unwrap();
            ids.push(id);
        }
        let removed = drop_acked(&pool, &[ids[0], ids[2], ids[4]]).await.unwrap();
        assert_eq!(removed, 3);
        let remaining: Vec<i64> = list_pending(&pool, 10)
            .await
            .unwrap()
            .iter()
            .map(|r| r.lamport_ts)
            .collect();
        assert_eq!(remaining, vec![2, 4]);
    }

    #[tokio::test]
    async fn drop_acked_empty_input_is_noop() {
        let pool = pool().await;
        enqueue(&pool, &draft("1", None), 1).await.unwrap();
        assert_eq!(drop_acked(&pool, &[]).await.unwrap(), 0);
        assert_eq!(count_pending(&pool).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn mark_failed_bumps_counter_and_records_error() {
        let pool = pool().await;
        let (id, _) = enqueue(&pool, &draft("1", None), 1).await.unwrap();
        mark_failed(&pool, id, "503 Service Unavailable")
            .await
            .unwrap();
        mark_failed(&pool, id, "504 Gateway Timeout").await.unwrap();
        let row = &list_pending(&pool, 1).await.unwrap()[0];
        assert_eq!(row.attempt_count, 2);
        assert_eq!(row.last_error.as_deref(), Some("504 Gateway Timeout"));
        assert!(row.last_attempt_at.is_some());
    }

    #[tokio::test]
    async fn check_constraint_rejects_unknown_op() {
        let pool = pool().await;
        let draft = PendingOpDraft {
            entity: "playlist".into(),
            entity_id: "1".into(),
            field: None,
            op: "rename".into(), // not in CHECK list
            payload: None,
        };
        let err = enqueue(&pool, &draft, 1).await.unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("check"));
    }

    #[tokio::test]
    async fn count_and_clear_observable() {
        let pool = pool().await;
        for lamport in 1..=3 {
            enqueue(&pool, &draft(&lamport.to_string(), None), lamport)
                .await
                .unwrap();
        }
        assert_eq!(count_pending(&pool).await.unwrap(), 3);
        let cleared = clear(&pool).await.unwrap();
        assert_eq!(cleared, 3);
        assert_eq!(count_pending(&pool).await.unwrap(), 0);
    }
}
