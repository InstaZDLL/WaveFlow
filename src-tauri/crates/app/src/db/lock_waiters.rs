//! Who is waiting for a profile database's write lock.
//!
//! SQLite keeps no queue for its write lock. A connection that finds it
//! taken runs its busy handler, which sleeps and asks again, and whoever asks
//! first after the lock is released takes it. A writer that holds the lock in
//! long stretches and takes it back as soon as it lets go — the scanner's
//! batches — leaves gaps that a sleeper almost never lands in: a click on a
//! track waited behind a scan until its five seconds ran out, and the queue it
//! was replacing was not written.
//!
//! So every pooled connection of a profile database gets [`install`]'s busy
//! handler in place of sqlx's `busy_timeout`. It sleeps exactly as SQLite's
//! own does and gives up at the same deadline, and on each sleep it notes that
//! a connection is waiting and wakes whatever listens for that. A long writer
//! asks [`waited_since`] (or awaits [`someone_waits`]) at its own stopping
//! points; when someone did wait, it commits and calls [`step_aside`] before
//! taking the lock back with [`begin_write`].
//!
//! Only the waiting is shared between connections, never whose wait it was:
//! a long writer compares the last wait with the moment it took the lock, so
//! its own wait to get the lock never counts.

use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use sqlx::sqlite::SqliteConnection;
use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::sync::Notify;

/// How long a statement waits for the lock before it fails with
/// `SQLITE_BUSY`. sqlx's default, which every pool had before this handler
/// replaced it.
pub const BUSY_TIMEOUT_MS: c_int = 5_000;

/// How long nobody must have waited before a writer that stepped aside takes
/// the lock back. Longer than the longest sleep of [`busy_delay`] (100 ms,
/// which a Windows timer can stretch by a tick), so a waiter has asked again
/// at least once since the lock was released.
const QUIET: Duration = Duration::from_millis(150);

/// The longest a writer steps aside. A waiter that is not waiting on it (on
/// another long transaction, say) must not stop it for good.
const MAX_STEP_ASIDE: Duration = Duration::from_secs(2);

/// How often [`step_aside`] looks whether the waiting is over.
const STEP_ASIDE_POLL: Duration = Duration::from_millis(25);

/// The clock every timestamp below counts from.
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// When a connection last slept waiting for a lock, in microseconds since
/// [`EPOCH`]. 0 means never.
static LAST_WAIT_US: AtomicU64 = AtomicU64::new(0);

/// Woken on every wait.
static WAITING: LazyLock<Notify> = LazyLock::new(Notify::new);

fn micros_since_epoch(at: Instant) -> u64 {
    at.saturating_duration_since(*EPOCH).as_micros() as u64
}

/// How long the busy handler sleeps on its `count`th call for one lock (0 on
/// the first), in milliseconds, or `None` once `timeout_ms` is spent. The
/// schedule of SQLite's own handler (`sqliteDefaultBusyCallback`), so a
/// connection waits neither more often nor longer than it did under sqlx's
/// `busy_timeout`.
fn busy_delay(count: c_int, timeout_ms: c_int) -> Option<c_int> {
    const DELAYS: [c_int; 12] = [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100];
    const TOTALS: [c_int; 12] = [0, 1, 3, 8, 18, 33, 53, 78, 103, 128, 178, 228];
    let count = count.max(0) as usize;
    let (mut delay, prior) = match DELAYS.get(count) {
        Some(&delay) => (delay, TOTALS[count]),
        None => {
            let last = DELAYS.len() - 1;
            let delay = DELAYS[last];
            let extra = (count - last) as i64 * delay as i64;
            (
                delay,
                (TOTALS[last] as i64 + extra).min(c_int::MAX as i64) as c_int,
            )
        }
    };
    if prior.saturating_add(delay) > timeout_ms {
        delay = timeout_ms - prior;
        if delay <= 0 {
            return None;
        }
    }
    Some(delay)
}

/// Note that a connection is waiting for a lock, now.
fn note_wait() {
    LAST_WAIT_US.store(micros_since_epoch(Instant::now()), Ordering::Release);
    WAITING.notify_one();
}

/// The busy handler. Runs on the connection's own thread (sqlx's worker),
/// inside `sqlite3_step`, which is why it sleeps rather than awaits — as
/// SQLite's own handler does.
unsafe extern "C" fn on_busy(_arg: *mut c_void, count: c_int) -> c_int {
    let Some(delay) = busy_delay(count, BUSY_TIMEOUT_MS) else {
        return 0;
    };
    note_wait();
    // SAFETY: `sqlite3_sleep` takes no connection and is safe from any thread.
    unsafe {
        libsqlite3_sys::sqlite3_sleep(delay);
    }
    1
}

/// Give `conn` the busy handler that reports its waits. For a pool's
/// `after_connect`; it replaces the `busy_timeout` sqlx installed, with the
/// same deadline ([`BUSY_TIMEOUT_MS`]).
pub async fn install(conn: &mut SqliteConnection) -> sqlx::Result<()> {
    // Started before any writer reads the clock, so no `Instant` taken from
    // here on can fall before it.
    LazyLock::force(&EPOCH);
    let mut handle = conn.lock_handle().await?;
    let db = handle.as_raw_handle().as_ptr();
    // SAFETY: `db` is the connection's open handle, held locked for the call;
    // the handler keeps no pointer to anything and its argument is unused.
    let rc =
        unsafe { libsqlite3_sys::sqlite3_busy_handler(db, Some(on_busy), std::ptr::null_mut()) };
    if rc != libsqlite3_sys::SQLITE_OK {
        return Err(sqlx::Error::Protocol(format!(
            "sqlite3_busy_handler failed with code {rc}"
        )));
    }
    Ok(())
}

/// Whether any connection has waited for a lock since `since`.
pub fn waited_since(since: Instant) -> bool {
    LAST_WAIT_US.load(Ordering::Acquire) > micros_since_epoch(since)
}

/// Resolves once a connection has waited for a lock since `since`. Safe to
/// drop unresolved, as a branch of a `select!`.
pub async fn someone_waits(since: Instant) {
    loop {
        if waited_since(since) {
            return;
        }
        WAITING.notified().await;
    }
}

/// Let whoever is waiting through. Called with the lock released: returns
/// once nobody has waited for [`QUIET`], or after [`MAX_STEP_ASIDE`].
pub async fn step_aside() {
    let started = Instant::now();
    loop {
        tokio::time::sleep(STEP_ASIDE_POLL).await;
        let quiet_since = Instant::now().checked_sub(QUIET).unwrap_or(*EPOCH);
        if !waited_since(quiet_since) || started.elapsed() >= MAX_STEP_ASIDE {
            return;
        }
    }
}

/// A transaction on a profile database that holds its write lock from the
/// start.
///
/// A deferred transaction (sqlx's `begin`) that reads before it writes takes a
/// snapshot with the read; if another connection commits before the first
/// write, that write fails at once with `SQLITE_BUSY_SNAPSHOT`, which no busy
/// handler waits out. A scan batch failed that way the moment a click got a
/// queue written between two batches, and the whole scan with it.
///
/// Not `BEGIN IMMEDIATE`: that reserves every database of the connection,
/// and a profile connection has `app.db` attached, whose own pool would then
/// wait on every scan batch. A write that changes nothing in `main` takes
/// the profile database's lock alone, waiting for it in the busy handler
/// before any snapshot is taken. `profile_setting` is in every profile
/// database.
pub async fn begin_write(pool: &SqlitePool) -> sqlx::Result<Transaction<'static, Sqlite>> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE main.profile_setting SET key = key WHERE 0")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

#[cfg(test)]
#[path = "lock_waiters_tests.rs"]
mod tests;
