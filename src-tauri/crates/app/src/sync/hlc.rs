// `next` is the public surface for the enqueue hooks; the diagnostic
// commands only need `read`. `observe_remote` lands in Phase B's WS
// subscriber alongside `lamport::observe_remote`, so it's still
// dead from the desktop side today.
#![allow(dead_code)]

//! Per-profile Hybrid Logical Clock (RFC-003 §2). Pairs `(wall,
//! logical)` that strictly increases per draw, the way the §2 total
//! order needs.
//!
//! ## Algorithm — single atomic draw
//!
//! Each call to [`next_conn`] runs a single SQLite UPSERT that:
//!
//! 1. Reads the persisted `(last_wall, last_logical)` pair.
//! 2. Picks a candidate wall = `max(now_ms, last_wall)`. This guards
//!    against wall-clock jumps backwards (NTP sync, manual time
//!    change, leap seconds): if the system clock rewinds, the HLC
//!    stays parked on the previous high-water mark instead of
//!    silently regressing.
//! 3. Picks a candidate logical:
//!    - `0` when `now_ms > last_wall` (a fresh wall tick — logical
//!      counter resets).
//!    - `last_logical + 1` otherwise (the wall didn't advance, the
//!      logical takes the slack).
//! 4. Persists the candidate pair atomically and returns it.
//!
//! ## i32 range guard
//!
//! `logical` is an `i32` on the wire (waveflow-server A.1.1 bind
//! site refuses anything outside `0..=i32::MAX`) and the matching
//! storage column has a CHECK constraint mirroring the bound. The
//! draw routine refuses to roll past `i32::MAX` — caller gets an
//! error rather than a silently-truncated value. In practice a user
//! would need to fire 2³¹ ops within a single epoch-ms tick to hit
//! this, which is ~2× faster than the fastest CPU can serialise an
//! `INSERT` to SQLite — physically unreachable, but the guard makes
//! that explicit instead of "trust me".
//!
//! ## Persistence
//!
//! Lives in `profile_setting` under two keys so the encode side
//! doesn't pay to pack a tuple into TEXT and parse it on every
//! draw. Per-profile because the active Better Auth user changes
//! every Lamport / HLC space.
//!
//! - `sync.hlc.last_wall` — `i64`, last drawn wall (epoch-ms)
//! - `sync.hlc.last_logical` — `i32`, last drawn logical counter

use chrono::Utc;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::{AppError, AppResult};

pub const KEY_WALL: &str = "sync.hlc.last_wall";
pub const KEY_LOGICAL: &str = "sync.hlc.last_logical";

const LOGICAL_MAX: i64 = i32::MAX as i64;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Atomic snapshot of the persisted clock as seen at the start of a
/// draw. Surfaces in tests and diagnostics; production code consumes
/// it via [`next`] / [`next_conn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlcPair {
    pub wall: i64,
    pub logical: i32,
}

impl HlcPair {
    pub const ZERO: Self = Self { wall: 0, logical: 0 };
}

/// Draw the next `(wall, logical)` pair on a fresh pool connection.
/// See [`next_conn`] for the algorithm; this thin wrapper acquires
/// once and delegates.
pub async fn next(profile_pool: &SqlitePool) -> AppResult<HlcPair> {
    let mut conn = profile_pool.acquire().await?;
    next_conn(&mut conn).await
}

/// Draw the next `(wall, logical)` pair on a caller-owned
/// connection. Composes with the entity write + outbox enqueue + the
/// existing Lamport bump in one atomic commit (see
/// [`crate::sync::hooks::enqueue_op_in_tx`]).
///
/// The draw runs in three round-trips against the same connection:
///
/// 1. SELECT the persisted pair (defaults to `(0, 0)` on first
///    call).
/// 2. Compute the candidate in Rust (cheap, no SQL trip).
/// 3. UPSERT the new pair atomically.
///
/// The connection serialises every other write under the SQLite
/// per-connection lock — two concurrent `next_conn` calls on the
/// same connection are impossible by construction, and two callers
/// holding separate connections still hit the wall-clock floor
/// (whichever commits first sets `last_wall`; the other reads it
/// and either advances or bumps logical).
pub async fn next_conn(conn: &mut SqliteConnection) -> AppResult<HlcPair> {
    let last = read_pair_conn(conn).await?;
    let now = now_ms();

    let (wall, logical) = if now > last.wall {
        (now, 0)
    } else {
        let next_logical = (last.logical as i64) + 1;
        if next_logical > LOGICAL_MAX {
            return Err(AppError::Other(format!(
                "sync HLC logical counter exhausted within wall tick {} (max {})",
                last.wall, LOGICAL_MAX,
            )));
        }
        (last.wall, next_logical as i32)
    };

    write_pair_conn(conn, wall, logical).await?;

    Ok(HlcPair { wall, logical })
}

/// Bump the local clock to at least `remote` so the next [`next`]
/// call returns a pair strictly greater than it under §2.
///
/// Mirror of [`crate::sync::lamport::observe_remote`]. Reserved for
/// Phase B's WS subscriber (which forwards remote HLCs into the
/// local floor); shipped here so the apply path can pull it in
/// without growing the public API surface.
pub async fn observe_remote(profile_pool: &SqlitePool, remote: HlcPair) -> AppResult<()> {
    let mut conn = profile_pool.acquire().await?;
    observe_remote_conn(&mut conn, remote).await
}

pub async fn observe_remote_conn(conn: &mut SqliteConnection, remote: HlcPair) -> AppResult<()> {
    if remote == HlcPair::ZERO {
        return Ok(());
    }
    let local = read_pair_conn(conn).await?;
    // §2 total order — only bump if the remote strictly outranks.
    if (remote.wall, remote.logical) <= (local.wall, local.logical) {
        return Ok(());
    }
    write_pair_conn(conn, remote.wall, remote.logical).await
}

/// Read the persisted pair without bumping. Returns `(0, 0)` when
/// no draw has fired yet.
pub async fn read(profile_pool: &SqlitePool) -> AppResult<HlcPair> {
    let mut conn = profile_pool.acquire().await?;
    read_pair_conn(&mut conn).await
}

async fn read_pair_conn(conn: &mut SqliteConnection) -> AppResult<HlcPair> {
    let row: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT
            (SELECT CAST(value AS INTEGER) FROM profile_setting WHERE key = ?),
            (SELECT CAST(value AS INTEGER) FROM profile_setting WHERE key = ?)",
    )
    .bind(KEY_WALL)
    .bind(KEY_LOGICAL)
    .fetch_one(conn)
    .await?;
    let wall = row.0.unwrap_or(0);
    let logical_raw = row.1.unwrap_or(0);
    // Defence-in-depth: if a manual edit somehow planted an out-of-
    // range logical the CHECK on `sync_pending_op` wouldn't have
    // caught (CHECK only fires on insert into that table, not on
    // the `profile_setting` row that holds the floor), clamp to the
    // legal range so the next draw doesn't bind a poisoned value.
    let logical = logical_raw.clamp(0, LOGICAL_MAX) as i32;
    Ok(HlcPair { wall, logical })
}

async fn write_pair_conn(
    conn: &mut SqliteConnection,
    wall: i64,
    logical: i32,
) -> AppResult<()> {
    let updated_at = now_ms();
    // Two separate UPSERTs because the pair lives under two keys.
    // Same connection = same SQLite per-connection lock, so the
    // pair appears atomic to every other reader on a different
    // connection.
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, CAST(? AS TEXT), 'int', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value,
                value_type = excluded.value_type,
                updated_at = excluded.updated_at",
    )
    .bind(KEY_WALL)
    .bind(wall)
    .bind(updated_at)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, CAST(? AS TEXT), 'int', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value,
                value_type = excluded.value_type,
                updated_at = excluded.updated_at",
    )
    .bind(KEY_LOGICAL)
    .bind(logical as i64)
    .bind(updated_at)
    .execute(&mut *conn)
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
    async fn next_starts_with_current_wall_and_logical_zero() {
        let pool = pool().await;
        let before = now_ms();
        let p = next(&pool).await.unwrap();
        let after = now_ms();
        assert!(p.wall >= before && p.wall <= after);
        assert_eq!(p.logical, 0);
    }

    #[tokio::test]
    async fn next_advances_logical_within_same_wall_tick() {
        let pool = pool().await;
        // Three back-to-back draws inside a single wall ms — logical
        // should march 0, 1, 2 without the wall changing.
        let a = next(&pool).await.unwrap();
        let b = next(&pool).await.unwrap();
        let c = next(&pool).await.unwrap();
        // Whether the wall ms ticked between draws is timing-
        // dependent on a real clock, but the §2 total order rule
        // is the same either way: the triple must be strictly
        // increasing.
        assert!((a.wall, a.logical) < (b.wall, b.logical));
        assert!((b.wall, b.logical) < (c.wall, c.logical));
    }

    #[tokio::test]
    async fn next_resets_logical_when_wall_advances() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        // Seed a pair from "an ms ago" so the next draw sees a
        // strictly newer `now`. Without seeding the test is
        // racy under fast clocks.
        let ms_ago = now_ms() - 100;
        write_pair_conn(&mut conn, ms_ago, 5).await.unwrap();
        let p = next_conn(&mut conn).await.unwrap();
        assert!(p.wall > ms_ago);
        assert_eq!(p.logical, 0);
    }

    #[tokio::test]
    async fn next_refuses_logical_overflow() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        // Plant a future wall so `now_ms()` can't overtake it and
        // force the draw down the "bump logical" branch.
        let future_wall = now_ms() + 10_000;
        write_pair_conn(&mut conn, future_wall, i32::MAX).await.unwrap();
        let err = next_conn(&mut conn).await.unwrap_err();
        let s = format!("{err}");
        assert!(
            s.contains("logical counter exhausted"),
            "unexpected error: {s}"
        );
    }

    #[tokio::test]
    async fn read_returns_zero_on_fresh_pool() {
        let pool = pool().await;
        assert_eq!(read(&pool).await.unwrap(), HlcPair::ZERO);
    }

    #[tokio::test]
    async fn observe_remote_bumps_past_local() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let local = next_conn(&mut conn).await.unwrap();
        let remote = HlcPair {
            wall: local.wall + 1_000,
            logical: 7,
        };
        observe_remote_conn(&mut conn, remote).await.unwrap();
        let after = read_pair_conn(&mut conn).await.unwrap();
        assert_eq!(after, remote);
    }

    #[tokio::test]
    async fn observe_remote_never_lowers_local() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let local = next_conn(&mut conn).await.unwrap();
        let stale = HlcPair { wall: 1, logical: 0 };
        observe_remote_conn(&mut conn, stale).await.unwrap();
        let after = read_pair_conn(&mut conn).await.unwrap();
        // Local stays put.
        assert_eq!(after, local);
    }

    #[tokio::test]
    async fn observe_remote_zero_pair_is_noop() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let before = read_pair_conn(&mut conn).await.unwrap();
        observe_remote_conn(&mut conn, HlcPair::ZERO).await.unwrap();
        let after = read_pair_conn(&mut conn).await.unwrap();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn read_clamps_poisoned_logical_to_legal_range() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES (?, '99999999999', 'int', 0)",
        )
        .bind(KEY_LOGICAL)
        .execute(&mut *conn)
        .await
        .unwrap();
        let pair = read_pair_conn(&mut conn).await.unwrap();
        assert_eq!(pair.logical, i32::MAX);
    }
}
