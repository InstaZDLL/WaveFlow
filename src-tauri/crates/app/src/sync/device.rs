// Every helper here is part of the public surface for the future
// CRUD enqueue hooks (1.f.desktop.2b) and the drain task
// (1.f.desktop.4). Only `read` is consumed by the diagnostic
// commands this PR ships — keeping `ensure` behind a module-scoped
// allow lets the follow-up review against the final shape.
#![allow(dead_code)]

//! Stable per-install device id. The server pins
//! `(user_id, device_id, operation_id)` + `(user_id, device_id,
//! lamport_ts)` UNIQUEs against this value, so a fresh UUID would
//! break the idempotency + monotonicity invariants the queue's drain
//! pass relies on. We generate one lazily on first read and never
//! rotate it — a reinstall produces a new device id, which the server
//! treats as a brand-new device (correct behaviour, since the
//! previous install's Lamport history isn't recoverable anyway).
//!
//! Lives in `app_setting` (app-wide) rather than `profile_setting`
//! because the same physical desktop is the same device regardless of
//! which Better Auth account the active profile maps to. Two profiles
//! sharing a desktop should send ops under the same `device_id` — the
//! server keys on `(user_id, device_id, lamport_ts)`, so different
//! `user_id`s already give each profile its own Lamport space.

use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::AppResult;

/// `app_setting` key holding the persisted device id.
pub const KEY: &str = "sync.device_id";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Return the stable device id, generating + persisting a fresh
/// UUIDv4 on first call. Subsequent calls (across reboots) hit the
/// cached row — no entropy drawn.
///
/// Uses `INSERT … ON CONFLICT(key) DO NOTHING RETURNING value` so a
/// race between two concurrent first-callers collapses to a single
/// row: the loser's INSERT is a no-op and the SELECT below picks up
/// the winner's id.
pub async fn ensure(app_db: &SqlitePool) -> AppResult<String> {
    if let Some(existing) = read(app_db).await? {
        return Ok(existing);
    }
    let new_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO NOTHING",
    )
    .bind(KEY)
    .bind(&new_id)
    .bind(now_ms())
    .execute(app_db)
    .await?;
    // Re-read to settle the race — the row above may have come from
    // a concurrent caller's INSERT that won the ON CONFLICT race.
    Ok(read(app_db).await?.unwrap_or(new_id))
}

/// Read the persisted device id without generating one. Returns
/// `None` only on a fresh install before [`ensure`] has been called.
pub async fn read(app_db: &SqlitePool) -> AppResult<Option<String>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY)
        .fetch_optional(app_db)
        .await?;
    Ok(raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// Stand up an in-memory sqlite pool with the minimal
    /// `app_setting` schema the device helpers need. Using a stripped
    /// schema (rather than running the real migrator) keeps the test
    /// fast and avoids dragging the entire `app.db` initial-state
    /// fixture into the unit suite.
    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE app_setting (
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
    async fn ensure_persists_uuid_and_is_idempotent() {
        let pool = pool().await;
        let first = ensure(&pool).await.unwrap();
        // The returned id must parse as a valid UUID.
        Uuid::parse_str(&first).expect("device id parses as UUID");
        // Subsequent calls return the same id — no entropy redraw,
        // no row replacement.
        let second = ensure(&pool).await.unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn read_returns_none_on_fresh_db() {
        let pool = pool().await;
        assert!(read(&pool).await.unwrap().is_none());
    }
}
