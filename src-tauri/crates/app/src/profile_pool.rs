use sqlx::SqlitePool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// How long a profile switch waits for outstanding leases before it
/// force-closes the previous pool anyway.
///
/// A leaked or genuinely long-running lease (a full library scan holds
/// its pool for minutes) must never wedge profile switching, so the
/// wait is bounded. Hitting the bound degrades to the pre-lease
/// behaviour — the close races whatever is still running — which is
/// exactly the `PoolClosed` window this mechanism exists to shrink, so
/// it is logged at WARN rather than swallowed.
const LEASE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Refcount of live [`ProfileLease`] handles for one profile epoch.
///
/// Lives behind an `Arc` shared by the [`ActiveProfile`] and every
/// lease it hands out, so a lease keeps the counter reachable even
/// after the profile has been swapped out of
/// [`AppState::profile`](crate::state::AppState::profile).
#[derive(Default)]
struct LeaseTracker {
    active: AtomicUsize,
    idle: Notify,
}

impl LeaseTracker {
    fn acquire(self: &Arc<Self>) -> ProfileLease {
        self.active.fetch_add(1, Ordering::AcqRel);
        ProfileLease {
            tracker: Arc::clone(self),
        }
    }

    /// Resolve once no lease is outstanding. Returns `false` if the
    /// deadline elapsed first.
    async fn wait_idle(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                // `enable()` registers this waiter *before* the count
                // is read. Creating the future is not enough — tokio
                // registers on first poll, so without this a lease
                // dropped between the load and the await would notify
                // nobody and we would block until the timeout.
                let notified = self.idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();

                if self.active.load(Ordering::Acquire) == 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .is_ok()
    }
}

/// Keeps a profile's pool open for as long as the holder needs it.
///
/// Handed out by [`AppState::require_profile_pool`](crate::state::AppState::require_profile_pool) /
/// [`AppState::require_profile_snapshot`](crate::state::AppState::require_profile_snapshot) as part of [`ProfilePool`],
/// and released on drop — including on the error paths of a `?`, since
/// that is just an early scope exit.
pub struct ProfileLease {
    tracker: Arc<LeaseTracker>,
}

impl Drop for ProfileLease {
    fn drop(&mut self) {
        if self.tracker.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Last lease out: wake any profile switch parked in
            // `wait_idle`. `notify_waiters` (not `notify_one`) because
            // a switch and a shutdown can both be waiting on the same
            // epoch, and a stored permit would leak to the next
            // acquirer.
            self.tracker.idle.notify_waiters();
        }
    }
}

/// A profile pool plus the lease that keeps it open.
///
/// Derefs to the inner [`SqlitePool`], so it passes anywhere a
/// `&SqlitePool` is expected. sqlx's query methods take a generic
/// `E: Executor` though, and deref coercion does not fire against a
/// type variable — those call sites need an explicit `&*pool`.
pub struct ProfilePool {
    pool: SqlitePool,
    _lease: ProfileLease,
}

impl std::ops::Deref for ProfilePool {
    type Target = SqlitePool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

impl ProfilePool {
    /// Split into the raw pool and its lease.
    ///
    /// For the handful of call sites that must hand an owned
    /// `SqlitePool` to a type living in `waveflow-core` (which knows
    /// nothing about app-layer leases). Keep the returned lease alive
    /// alongside that value — [`Leased`] does exactly that.
    pub fn into_parts(self) -> (SqlitePool, ProfileLease) {
        (self.pool, self._lease)
    }

    /// Give up lease tracking and keep only the pool.
    ///
    /// Reserved for long-lived handles held by a worker for the life of
    /// the process rather than for the span of one command — leasing
    /// those would stall every profile switch until
    /// `LEASE_DRAIN_TIMEOUT` for no benefit. Such a handle is back to
    /// the pre-#332 exposure and must tolerate `PoolClosed`.
    ///
    /// The only caller is the DLNA worker, which today keeps serving
    /// from the *previous* profile's (now closed) pool after a switch —
    /// a pre-existing gap, not something the lease changes either way.
    /// See issue #399.
    pub fn into_unleashed(self) -> SqlitePool {
        self.pool
    }
}

/// A value built from a leased pool, carrying the lease with it.
///
/// Derefs to the inner value, so repository helpers can keep returning
/// "a repository" while the lease rides along invisibly and releases
/// when the caller drops it.
pub struct Leased<T> {
    inner: T,
    _lease: ProfileLease,
}

impl<T> Leased<T> {
    pub fn new(inner: T, lease: ProfileLease) -> Self {
        Self {
            inner,
            _lease: lease,
        }
    }
}

impl<T> std::ops::Deref for Leased<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for Leased<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Active per-profile database. Closed and replaced on profile switch.
pub struct ActiveProfile {
    pub profile_id: i64,
    pub pool: SqlitePool,
    leases: Arc<LeaseTracker>,
}

impl ActiveProfile {
    pub(crate) fn new(profile_id: i64, pool: SqlitePool) -> Self {
        Self {
            profile_id,
            pool,
            leases: Arc::new(LeaseTracker::default()),
        }
    }

    pub(crate) fn lease(&self) -> ProfilePool {
        ProfilePool {
            pool: self.pool.clone(),
            _lease: self.leases.acquire(),
        }
    }

    /// Wait for outstanding leases to drain, then close the pool.
    ///
    /// Called only after this epoch has been swapped out of
    /// [`AppState::profile`](crate::state::AppState::profile), so no new lease can be issued against it
    /// and the count is monotonically decreasing.
    pub(crate) async fn close_when_idle(self) {
        if !self.leases.wait_idle(LEASE_DRAIN_TIMEOUT).await {
            tracing::warn!(
                profile_id = self.profile_id,
                outstanding = self.leases.active.load(Ordering::Acquire),
                timeout_secs = LEASE_DRAIN_TIMEOUT.as_secs(),
                "profile leases still held at switch; closing pool anyway",
            );
        }
        self.pool.close().await;
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    use std::time::Instant;

    /// Stand-in for an active profile backed by an in-memory database.
    /// Exercises the real [`ActiveProfile`] lease + close path without
    /// needing an `AppHandle` (an `AppState` cannot be built in a unit
    /// test).
    async fn active_profile() -> ActiveProfile {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        ActiveProfile::new(1, pool)
    }

    #[tokio::test]
    async fn wait_idle_returns_immediately_without_leases() {
        let tracker = Arc::new(LeaseTracker::default());
        assert!(tracker.wait_idle(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn outstanding_lease_blocks_the_drain() {
        let tracker = Arc::new(LeaseTracker::default());
        let lease = tracker.acquire();

        assert!(
            !tracker.wait_idle(Duration::from_millis(50)).await,
            "drain must time out while a lease is held",
        );

        drop(lease);
        assert!(tracker.wait_idle(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn every_lease_must_drop_before_the_drain_completes() {
        let tracker = Arc::new(LeaseTracker::default());
        let first = tracker.acquire();
        let second = tracker.acquire();

        drop(first);
        assert!(
            !tracker.wait_idle(Duration::from_millis(50)).await,
            "one of two leases released is not idle",
        );

        drop(second);
        assert!(tracker.wait_idle(Duration::from_millis(50)).await);
    }

    /// The waiter must be woken by the drop, not by polling — so the
    /// drain resolves promptly rather than sitting out its timeout.
    #[tokio::test]
    async fn dropping_the_last_lease_wakes_a_parked_drain() {
        let tracker = Arc::new(LeaseTracker::default());
        let lease = tracker.acquire();

        let holder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(lease);
        });

        let started = Instant::now();
        assert!(tracker.wait_idle(Duration::from_secs(10)).await);
        let waited = started.elapsed();

        assert!(
            waited >= Duration::from_millis(90),
            "drain resolved before the lease was released ({waited:?})",
        );
        assert!(
            waited < Duration::from_secs(5),
            "drain waited far past the release, so it was not woken ({waited:?})",
        );
        holder.await.unwrap();
    }

    /// The regression this whole mechanism exists for (issue #332): a
    /// multi-step command holds a leased pool across an await while a
    /// profile switch closes the epoch. The command's later query must
    /// still succeed instead of failing with `PoolClosed`.
    #[tokio::test]
    async fn leased_pool_survives_a_concurrent_profile_switch() {
        let profile = active_profile().await;
        let leased = profile.lease();

        // The switch takes the epoch out of `AppState::profile` and
        // drains it. Runs concurrently with the command below.
        let switch = tokio::spawn(async move { profile.close_when_idle().await });

        // Step 1 of the command, then an await that straddles the switch.
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&*leased)
            .await
            .expect("first step");
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Step 2 — this is what used to blow up with `PoolClosed`.
        let value: i64 = sqlx::query_scalar("SELECT 2")
            .fetch_one(&*leased)
            .await
            .expect("second step must not hit PoolClosed");
        assert_eq!(value, 2);

        assert!(!switch.is_finished(), "switch closed the pool under us");
        drop(leased);
        switch.await.unwrap();
    }

    /// Once the lease is released the pool really is closed — the drain
    /// must not leak the old epoch.
    #[tokio::test]
    async fn pool_closes_after_the_lease_is_released() {
        let profile = active_profile().await;
        let leased = profile.lease();
        let observer = (*leased).clone();

        let switch = tokio::spawn(async move { profile.close_when_idle().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!observer.is_closed());

        drop(leased);
        switch.await.unwrap();
        assert!(observer.is_closed());
    }

    /// A lease taken from the *new* epoch must not hold the old one
    /// open: each `ActiveProfile` owns its own tracker.
    #[tokio::test]
    async fn epochs_track_leases_independently() {
        let previous = active_profile().await;
        let next = active_profile().await;

        let leased_next = next.lease();
        // Nothing outstanding on `previous`, so its drain is immediate
        // even though the new epoch is leased.
        assert!(previous.leases.wait_idle(Duration::from_millis(50)).await);
        assert!(!next.leases.wait_idle(Duration::from_millis(50)).await);
        drop(leased_next);
    }
}
