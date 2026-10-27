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
