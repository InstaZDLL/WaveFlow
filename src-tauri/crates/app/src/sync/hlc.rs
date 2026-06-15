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

use std::sync::LazyLock;

use chrono::Utc;
use sqlx::{SqliteConnection, SqlitePool};
use tokio::sync::Mutex;

use crate::error::{AppError, AppResult};

/// Process-wide mutex serialising the read-modify-write cycle that
/// makes up an HLC draw. The two-step nature of the draw —
/// `read_pair_conn` SELECT followed by `write_pair_conn` SAVEPOINT
/// — is not atomic at the SQLite layer when the caller does not
/// already hold a write lock: two concurrent callers on sibling
/// connections can read the same baseline, compute the same
/// candidate, and both UPSERT it, returning duplicate pairs that
/// would break the RFC-003 §2 total order.
///
/// The mutex closes that window for every caller that goes through
/// [`next_conn`] or [`observe_remote_conn`]. In the production
/// hook path the mutex is technically redundant (the caller's
/// `Transaction<'_, Sqlite>` already serialises HLC writes via the
/// SQLite write lock once the entity write fires), but it stays
/// cheap (sub-microsecond contention) and gives the public API a
/// race-free contract that doesn't depend on caller discipline.
///
/// Single mutex per process is enough — the desktop runs at most
/// one active profile at a time, and cross-profile HLC contention
/// is rare enough that per-pool granularity would buy nothing
/// while complicating the ownership story.
static HLC_DRAW_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
/// 3. UPSERT the new pair through a SAVEPOINT so both rows land or
///    none.
///
/// [`HLC_DRAW_LOCK`] wraps the whole cycle. Without it, two
/// concurrent callers on sibling connections could each
/// read-compute-write the same baseline and return duplicate
/// pairs — a §2 total-order violation. The mutex is held for the
/// duration of the three round-trips (microseconds in practice).
pub async fn next_conn(conn: &mut SqliteConnection) -> AppResult<HlcPair> {
    let _guard = HLC_DRAW_LOCK.lock().await;

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
    // Same serialisation rationale as `next_conn` — without the
    // lock, a concurrent `next_conn` (or another `observe_remote`)
    // could read between this read and the write_pair_conn UPSERT,
    // missing the bump and shipping a stale pair on the next draw.
    let _guard = HLC_DRAW_LOCK.lock().await;
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
    // RFC-003 §2 requires the pair to be observed as a unit by any
    // reader. Two bare UPSERTs auto-commit independently in
    // autocommit mode — a concurrent reader on a sibling
    // connection could then observe `wall` bumped while `logical`
    // still holds the previous value, breaking the total-order
    // invariant.
    //
    // SAVEPOINT wraps the two UPSERTs into a single
    // observable-as-a-unit step regardless of the caller's tx state:
    //
    // - From `enqueue_op_in_tx`, the caller already holds a
    //   `Transaction<'_, Sqlite>` and the SAVEPOINT nests inside
    //   it (SQLite supports unlimited nesting of savepoints).
    //   `RELEASE SAVEPOINT` then defers the durable commit to the
    //   outer caller's `tx.commit()`.
    // - From the diagnostic / test path (`next` / `observe_remote`
    //   wrappers that acquire a fresh pool connection in autocommit
    //   mode), the SAVEPOINT acts as an implicit transaction —
    //   `RELEASE` commits both writes atomically.
    //
    // Either way `now_ms()` is sampled once so the two rows agree
    // on `updated_at` (the bare-UPSERT version sampled twice, which
    // could differ by 1 ms across the round-trip pair).
    let updated_at = now_ms();
    sqlx::query("SAVEPOINT hlc_write")
        .execute(&mut *conn)
        .await?;

    let inner = async {
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
        Ok::<_, AppError>(())
    }
    .await;

    match inner {
        Ok(()) => {
            sqlx::query("RELEASE SAVEPOINT hlc_write")
                .execute(&mut *conn)
                .await?;
            Ok(())
        }
        Err(e) => {
            // Best-effort rollback to the savepoint — the original
            // error is the one the caller cares about. `ROLLBACK TO
            // SAVEPOINT … ; RELEASE SAVEPOINT …` is the SQLite
            // dance to undo just the inner step without disturbing
            // an outer transaction.
            let _ = sqlx::query("ROLLBACK TO SAVEPOINT hlc_write")
                .execute(&mut *conn)
                .await;
            let _ = sqlx::query("RELEASE SAVEPOINT hlc_write")
                .execute(&mut *conn)
                .await;
            Err(e)
        }
    }
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
    async fn next_serialises_concurrent_callers_on_sibling_connections() {
        // Two concurrent draws through the public pool-level `next`
        // — each acquires its own connection. Without the
        // module-level mutex they would race the read/write cycle
        // and could return duplicate pairs. The mutex makes the
        // draws strictly ordered, so the returned pairs are
        // distinct under the §2 total order.
        //
        // Built inline rather than via the shared `pool()` helper
        // because we need `max_connections > 1` so the two
        // `acquire()` calls inside concurrent `next` callers don't
        // queue at the pool level (which would mask the actual
        // mutex behaviour).
        let multi_pool = SqlitePoolOptions::new()
            .max_connections(4)
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
        .execute(&multi_pool)
        .await
        .unwrap();

        // Fire 8 draws concurrently. The mutex must give each one
        // a distinct triple under §2. We collect them and assert
        // distinctness via a HashSet on the derived ordering key.
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let pool = multi_pool.clone();
                tokio::spawn(async move { next(&pool).await.unwrap() })
            })
            .collect();
        let mut pairs = Vec::with_capacity(handles.len());
        for h in handles {
            pairs.push(h.await.unwrap());
        }
        let unique: std::collections::HashSet<(i64, i32)> =
            pairs.iter().map(|p| (p.wall, p.logical)).collect();
        assert_eq!(
            unique.len(),
            pairs.len(),
            "all concurrent draws must return distinct (wall, logical) pairs; got {pairs:?}",
        );
    }

    #[tokio::test]
    async fn next_conn_composes_with_caller_owned_transaction() {
        // The hook path (`sync::hooks::enqueue_op_in_tx`) opens its
        // own tx on the borrowed connection and expects the inner
        // `next_conn` to nest cleanly. SAVEPOINT inside an active
        // transaction is the SQLite-supported pattern; verify the
        // composition end-to-end so a future refactor that swaps
        // SAVEPOINT for `BEGIN IMMEDIATE` (which would fail with
        // "cannot start a transaction within a transaction") trips
        // this test before production.
        let pool = pool().await;
        let mut tx = pool.begin().await.unwrap();
        let pair = next_conn(&mut tx).await.unwrap();
        // The pair lands on the row even though the outer tx is
        // still open — same connection, savepoint released, but
        // the outer tx commit is deferred.
        tx.commit().await.unwrap();
        let after = read(&pool).await.unwrap();
        assert_eq!(after, pair);
    }

    #[tokio::test]
    async fn next_conn_two_writes_in_one_outer_tx_round_trip() {
        // Two consecutive draws inside a single outer transaction
        // must both land atomically when the outer commits.
        let pool = pool().await;
        let mut tx = pool.begin().await.unwrap();
        let a = next_conn(&mut tx).await.unwrap();
        let b = next_conn(&mut tx).await.unwrap();
        assert!((a.wall, a.logical) < (b.wall, b.logical));
        tx.commit().await.unwrap();
        // The persisted row holds the LATER pair — the earlier
        // savepoint was released into the outer tx, so the outer
        // commit promotes both writes' net effect.
        let after = read(&pool).await.unwrap();
        assert_eq!(after, b);
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
