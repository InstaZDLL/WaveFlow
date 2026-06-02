//! Per-profile pull cursor — the last `sync_op.id` this device has
//! applied + ACKed upstream. Phase 1.f.desktop.4b.
//!
//! Persisted in `profile_setting['sync.last_seen_id']` (seeded by the
//! `20260603000000_sync_canonical_id` migration). The WS subscriber
//! advances the value after every successfully-applied op (whether
//! the op arrived via WS push or via the catch-up REST pull) and
//! re-sends `{"ack": N}` to the server so the device's cursor row
//! climbs at the same pace.
//!
//! The cursor is the source of truth for "where am I in the log on
//! reconnect" — the server's `GET /api/v1/sync/ops?since=N` resumes
//! from exactly this value, and the resurrected-device guard
//! (compaction watermark vs `since`) is what triggers a full resync
//! when the value has fallen too far behind.

use chrono::Utc;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::AppResult;

/// `profile_setting` key holding the last applied op id.
pub const KEY: &str = "sync.last_seen_id";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Read the cursor for the active profile. Returns `0` (= "send me
/// the whole log") when the row hasn't been seeded yet — covers a
/// pre-migration profile activated on a freshly-updated install.
pub async fn read(profile_pool: &SqlitePool) -> AppResult<i64> {
    let raw: Option<i64> =
        sqlx::query_scalar("SELECT CAST(value AS INTEGER) FROM profile_setting WHERE key = ?")
            .bind(KEY)
            .fetch_optional(profile_pool)
            .await?;
    Ok(raw.unwrap_or(0))
}

/// Reset the cursor to 0 (= "send me the whole log on next pull").
/// Used by the WS subscriber's 410 Gone handler — when the server's
/// compaction watermark climbs past our cursor we can't pull from
/// `since=N` cleanly, so we drop the row and the next [`read`]
/// returns the default 0. Centralised here so any future "reset"
/// side-effects (logging, eviction events) land in one place rather
/// than against an inline SQL DELETE at the call site.
pub async fn reset(profile_pool: &SqlitePool) -> AppResult<()> {
    sqlx::query("DELETE FROM profile_setting WHERE key = ?")
        .bind(KEY)
        .execute(profile_pool)
        .await?;
    Ok(())
}

/// Advance the cursor in the caller's transaction. Idempotent via
/// the `max(...)` clamp: a stale advance (e.g. a catch-up batch
/// processing a row the WS already applied) never drags the value
/// backwards.
///
/// Same TEXT-affinity defence the Lamport bump uses — both sides of
/// the `max()` go through `CAST AS INTEGER` so `max(10, "3")`
/// returns `10`, not the lexically-larger `"3"`.
pub async fn advance_conn(conn: &mut SqliteConnection, new_value: i64) -> AppResult<()> {
    if new_value <= 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, CAST(? AS TEXT), 'int', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = CAST(
                    max(
                        CAST(profile_setting.value AS INTEGER),
                        CAST(excluded.value AS INTEGER)
                    ) AS TEXT
                ),
                updated_at = excluded.updated_at",
    )
    .bind(KEY)
    .bind(new_value)
    .bind(now_ms())
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE profile_setting (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                value_type TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn fresh_profile_reads_zero() {
        let pool = pool().await;
        assert_eq!(read(&pool).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn advance_is_monotonic() {
        let pool = pool().await;
        // Scope the conn so the trailing `read(&pool)` can grab the
        // only slot (`max_connections = 1` + `:memory:` rule — see
        // sync::canonical::tests for the long form).
        {
            let mut conn = pool.acquire().await.unwrap();
            advance_conn(&mut conn, 10).await.unwrap();
            advance_conn(&mut conn, 5).await.unwrap();
        }
        assert_eq!(read(&pool).await.unwrap(), 10);
        {
            let mut conn = pool.acquire().await.unwrap();
            advance_conn(&mut conn, 42).await.unwrap();
        }
        assert_eq!(read(&pool).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn reset_clears_row_so_next_read_is_zero() {
        let pool = pool().await;
        {
            let mut conn = pool.acquire().await.unwrap();
            advance_conn(&mut conn, 50).await.unwrap();
        }
        assert_eq!(read(&pool).await.unwrap(), 50);
        reset(&pool).await.unwrap();
        assert_eq!(read(&pool).await.unwrap(), 0);
        // Idempotent — second call still succeeds.
        reset(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn zero_or_negative_is_noop() {
        let pool = pool().await;
        {
            let mut conn = pool.acquire().await.unwrap();
            advance_conn(&mut conn, 0).await.unwrap();
            advance_conn(&mut conn, -5).await.unwrap();
        }
        assert_eq!(read(&pool).await.unwrap(), 0);
    }
}
