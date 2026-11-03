//! The runtime layer's contract, on every backend the build has.
//!
//! Each scenario is one async fn, run once under `#[tokio::test]` and once
//! under `smol::block_on` with no tokio runtime in reach, so the two arms are
//! held to exactly the same behavior. The task-handle scenarios matter most:
//! the crate's background tasks are written against "drop detaches, abort
//! stops", and smol's own `Task` does the opposite.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::*;

/// Generous bound for anything that should happen promptly.
const PROMPTLY: Duration = Duration::from_secs(5);

/// Sets its flag when dropped, so a test can see a task's future go away.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Wait (bounded) until `flag` is set.
async fn eventually(flag: &AtomicBool) -> bool {
    timeout(PROMPTLY, async {
        while !flag.load(Ordering::SeqCst) {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .is_ok()
}

async fn a_dropped_handle_leaves_the_task_running() {
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    let handle = spawn(async move {
        sleep(Duration::from_millis(20)).await;
        flag.store(true, Ordering::SeqCst);
    });
    drop(handle);
    assert!(
        eventually(&finished).await,
        "dropping the handle must detach, not cancel"
    );
}

async fn abort_stops_the_task_and_drops_its_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let (guard, flag) = (DropFlag(Arc::clone(&dropped)), Arc::clone(&finished));
    let handle = spawn(async move {
        let _guard = guard;
        sleep(Duration::from_secs(3600)).await;
        flag.store(true, Ordering::SeqCst);
    });
    // Let it start and park in the sleep.
    sleep(Duration::from_millis(20)).await;

    handle.abort();
    assert!(
        eventually(&dropped).await,
        "abort must drop the task's future"
    );
    assert!(!finished.load(Ordering::SeqCst));
}

async fn sleep_waits_its_duration() {
    let started = Instant::now();
    sleep(Duration::from_millis(30)).await;
    assert!(started.elapsed() >= Duration::from_millis(30));
}

async fn timeout_returns_a_finished_future_and_gives_up_on_a_stuck_one() {
    assert_eq!(timeout(PROMPTLY, async { 7 }).await, Ok(7));
    assert_eq!(
        timeout(Duration::from_millis(20), std::future::pending::<()>()).await,
        Err(Elapsed)
    );
}

async fn a_ready_future_wins_even_at_a_passed_deadline() {
    let passed = Instant::now() - Duration::from_millis(1);
    assert_eq!(timeout_at(passed, async { "done" }).await, Ok("done"));
}

async fn the_deadline_is_taken_when_timeout_is_called() {
    let waiting = timeout(Duration::from_millis(30), std::future::pending::<()>());
    sleep(Duration::from_millis(60)).await;
    let started = Instant::now();
    assert_eq!(waiting.await, Err(Elapsed));
    assert!(
        started.elapsed() < Duration::from_millis(30),
        "the deadline passed while nobody polled, so the first poll must give up at once"
    );
}

async fn an_unbounded_timeout_does_not_overflow() {
    assert_eq!(timeout(Duration::MAX, async { 1 }).await, Ok(1));
}

/// A yield is exactly one turn: pending once, having woken itself so the
/// executor comes back, then done. Executor-independent, so polled by hand.
#[test]
fn yield_now_steps_aside_once_and_wakes_itself() {
    use std::sync::atomic::AtomicUsize;
    use std::task::Wake;

    struct CountWakes(AtomicUsize);
    impl Wake for CountWakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let wakes = Arc::new(CountWakes(AtomicUsize::new(0)));
    let waker = Arc::clone(&wakes).into();
    let mut cx = Context::from_waker(&waker);
    let mut yielding = pin!(yield_now());

    assert!(yielding.as_mut().poll(&mut cx).is_pending());
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    assert!(yielding.as_mut().poll(&mut cx).is_ready());
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "tokio")]
mod on_tokio {
    use super::*;

    #[tokio::test]
    async fn a_dropped_handle_leaves_the_task_running() {
        super::a_dropped_handle_leaves_the_task_running().await;
    }

    #[tokio::test]
    async fn abort_stops_the_task_and_drops_its_future() {
        super::abort_stops_the_task_and_drops_its_future().await;
    }

    #[tokio::test]
    async fn sleep_waits_its_duration() {
        super::sleep_waits_its_duration().await;
    }

    #[tokio::test]
    async fn timeout_returns_a_finished_future_and_gives_up_on_a_stuck_one() {
        super::timeout_returns_a_finished_future_and_gives_up_on_a_stuck_one().await;
    }

    #[tokio::test]
    async fn a_ready_future_wins_even_at_a_passed_deadline() {
        super::a_ready_future_wins_even_at_a_passed_deadline().await;
    }

    #[tokio::test]
    async fn the_deadline_is_taken_when_timeout_is_called() {
        super::the_deadline_is_taken_when_timeout_is_called().await;
    }

    #[tokio::test]
    async fn an_unbounded_timeout_does_not_overflow() {
        super::an_unbounded_timeout_does_not_overflow().await;
    }

    /// Tokio's paused clock drives our timers, which is what the crate's
    /// timing tests (`download_tests`, `upload_tests`) rely on.
    #[tokio::test(start_paused = true)]
    async fn a_paused_tokio_clock_drives_the_timers() {
        let started = Instant::now();
        sleep(Duration::from_secs(3600)).await;
        assert!(started.elapsed() >= Duration::from_secs(3600));
    }

    #[tokio::test]
    async fn a_tokio_runtime_runs_on_tokio() {
        assert_eq!(backend(), Backend::Tokio);
    }

    #[tokio::test]
    async fn try_spawn_runs_the_task_inside_a_runtime() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        try_spawn(async move { flag.store(true, Ordering::SeqCst) })
            .expect("a runtime is in reach")
            .detach();
        assert!(eventually(&finished).await);
    }

    /// `Drop` calls it from wherever the owner let go, so it answers `None`
    /// where `spawn` would panic, and never panics itself.
    #[cfg(not(feature = "smol"))]
    #[test]
    fn try_spawn_outside_any_runtime_declines_instead_of_panicking() {
        assert!(try_spawn(async {}).is_none());
    }
}

#[cfg(feature = "smol")]
mod on_smol {
    use super::*;

    fn run(test: impl std::future::Future<Output = ()>) {
        smol::block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_err());
            test.await;
        });
    }

    #[test]
    fn a_dropped_handle_leaves_the_task_running() {
        run(super::a_dropped_handle_leaves_the_task_running());
    }

    #[test]
    fn abort_stops_the_task_and_drops_its_future() {
        run(super::abort_stops_the_task_and_drops_its_future());
    }

    #[test]
    fn sleep_waits_its_duration() {
        run(super::sleep_waits_its_duration());
    }

    #[test]
    fn timeout_returns_a_finished_future_and_gives_up_on_a_stuck_one() {
        run(super::timeout_returns_a_finished_future_and_gives_up_on_a_stuck_one());
    }

    #[test]
    fn a_ready_future_wins_even_at_a_passed_deadline() {
        run(super::a_ready_future_wins_even_at_a_passed_deadline());
    }

    #[test]
    fn the_deadline_is_taken_when_timeout_is_called() {
        run(super::the_deadline_is_taken_when_timeout_is_called());
    }

    #[test]
    fn an_unbounded_timeout_does_not_overflow() {
        run(super::an_unbounded_timeout_does_not_overflow());
    }

    #[test]
    fn no_tokio_runtime_runs_on_smol() {
        run(async { assert_eq!(backend(), Backend::Smol) });
    }
}
