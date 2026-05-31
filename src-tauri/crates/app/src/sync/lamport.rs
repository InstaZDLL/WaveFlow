// See the matching note in `device.rs` — `next` / `observe_remote`
// are the public surface the CRUD hooks + WS subscriber consume in
// follow-up sub-PRs. Diagnostic commands here use only `read`.
#![allow(dead_code)]

//! Per-profile Lamport clock. The server's
//! `(user_id, device_id, lamport_ts)` UNIQUE rejects regressions with
//! SQLSTATE 23505, so the only way for the desktop to keep its
//! batches accepted is to draw a strictly-increasing value for every
//! local op AND bump past every remote op the WS subscriber forwards.
//!
//! Persistence lives in `profile_setting['sync.lamport_local_max']`
//! (per-profile because each profile maps to a distinct Better Auth
//! user, and the server keys its UNIQUE on `(user_id, device_id,
//! lamport_ts)` — different users get their own Lamport spaces even
//! on the same device).
//!
//! Both writes are atomic via a single SQLite UPSERT + RETURNING.
//! No `SELECT … then UPDATE` race window between concurrent
//! enqueue calls.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::AppResult;

/// `profile_setting` key holding the per-profile Lamport floor.
pub const KEY: &str = "sync.lamport_local_max";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Atomically increment the Lamport clock and return the new value.
///
/// Implemented as a single UPSERT with `RETURNING`:
///
/// - Fresh profile (no row yet) → INSERT with `value = '1'`, return
///   `1`.
/// - Existing row → DO UPDATE bumps the value by 1, return the new
///   value.
///
/// The CAST chains keep the column as TEXT (which is what the
/// `profile_setting` schema requires for compat with every other
/// setting type) while operating arithmetically.
pub async fn next(profile_pool: &SqlitePool) -> AppResult<i64> {
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, '1', 'int', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = CAST(CAST(profile_setting.value AS INTEGER) + 1 AS TEXT),
                updated_at = excluded.updated_at
         RETURNING CAST(value AS INTEGER)",
    )
    .bind(KEY)
    .bind(now_ms())
    .fetch_one(profile_pool)
    .await?;
    Ok(row.0)
}

/// Bump the local clock to at least `remote` so the next [`next`]
/// call returns `remote + 1`. No-op when the local value is already
/// at or above `remote`.
///
/// Used by the future WS subscriber (1.f.desktop.4): every inbound
/// `sync_op` carries the originating device's `lamport_ts`; observing
/// it locally keeps the Lamport ordering coherent across devices so
/// the next local op the user fires can't slot below a remote op the
/// server has already committed.
pub async fn observe_remote(profile_pool: &SqlitePool, remote: i64) -> AppResult<()> {
    if remote <= 0 {
        return Ok(());
    }
    // SQLite's scalar `max(a, b, …)` returns the greatest argument
    // (distinct from the `max(col)` aggregate). On INSERT (no existing
    // row) the value is just `remote`; on UPDATE we take the
    // pairwise max so a stale remote can't lower the local floor.
    //
    // Both sides MUST be cast to INTEGER before the `max()` call —
    // SQLite's type-affinity rules make TEXT > INTEGER in a mixed
    // comparison, so `max(10, "3")` would return `"3"` and silently
    // lower the local floor. The explicit double cast is what keeps
    // the monotonicity invariant intact.
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
    .bind(remote)
    .bind(now_ms())
    .execute(profile_pool)
    .await?;
    Ok(())
}

/// Read the current Lamport floor without bumping it. Returns `0`
/// when the row hasn't been created yet (= no local op has ever
/// fired, the clock is at its natural origin). Diagnostic only —
/// production code should always go through [`next`] /
/// [`observe_remote`] so the increments stay atomic.
pub async fn read(profile_pool: &SqlitePool) -> AppResult<i64> {
    let raw: Option<i64> =
        sqlx::query_scalar("SELECT CAST(value AS INTEGER) FROM profile_setting WHERE key = ?")
            .bind(KEY)
            .fetch_optional(profile_pool)
            .await?;
    Ok(raw.unwrap_or(0))
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
    async fn next_starts_at_one_and_is_monotonic() {
        let pool = pool().await;
        assert_eq!(next(&pool).await.unwrap(), 1);
        assert_eq!(next(&pool).await.unwrap(), 2);
        assert_eq!(next(&pool).await.unwrap(), 3);
        assert_eq!(read(&pool).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn observe_remote_bumps_past_local() {
        let pool = pool().await;
        assert_eq!(next(&pool).await.unwrap(), 1);
        observe_remote(&pool, 42).await.unwrap();
        // Local is now 42 — next() must return 43, not 2.
        assert_eq!(next(&pool).await.unwrap(), 43);
    }

    #[tokio::test]
    async fn observe_remote_never_lowers_local() {
        let pool = pool().await;
        for _ in 0..10 {
            next(&pool).await.unwrap();
        }
        observe_remote(&pool, 3).await.unwrap();
        // Local stays at 10 (the latest next() return) — a stale
        // remote can't drag the clock backwards.
        assert_eq!(read(&pool).await.unwrap(), 10);
        assert_eq!(next(&pool).await.unwrap(), 11);
    }

    #[tokio::test]
    async fn observe_remote_handles_zero_and_negative() {
        let pool = pool().await;
        observe_remote(&pool, 0).await.unwrap();
        observe_remote(&pool, -5).await.unwrap();
        // Neither call should have planted a row.
        assert_eq!(read(&pool).await.unwrap(), 0);
        assert_eq!(next(&pool).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn observe_remote_seeds_clock_on_fresh_profile() {
        let pool = pool().await;
        // No prior `next()` — observing a remote should still plant
        // the row so the next local op slots above it.
        observe_remote(&pool, 7).await.unwrap();
        assert_eq!(read(&pool).await.unwrap(), 7);
        assert_eq!(next(&pool).await.unwrap(), 8);
    }
}
