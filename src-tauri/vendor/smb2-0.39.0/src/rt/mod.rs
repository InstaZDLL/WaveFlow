//! The async runtime underneath the client: spawning tasks, timers, and
//! sockets, on tokio or on smol.
//!
//! Everything in the crate that needs a running reactor goes through here.
//! What doesn't need one stays plain tokio on purpose: `tokio::sync`
//! (channels, semaphores, `Notify`) works on any executor.
//!
//! **A smol-only build is the proof.** It leaves tokio's `time`, `net`,
//! `io-util`, and `rt` features off, so a `tokio::time::sleep` or
//! `tokio::spawn` anywhere outside this module fails to compile there
//! instead of panicking at runtime. Keep that build in the checks.
//!
//! **Which backend runs a call is decided per call**, by [`backend`]: a tokio
//! runtime in reach wins, and without one the call runs on smol. So with only
//! one feature on there is nothing to decide, and with both on (feature
//! unification makes that common) each consumer gets the runtime it is
//! actually running on. A call only ever sees one backend, because a
//! connection's tasks, timers, and socket are all created from the same
//! context.
//!
//! See `CLAUDE.md` beside this file for the traps, the task-handle one first.

#[cfg(not(any(feature = "tokio", feature = "smol")))]
compile_error!(
    "smb2 needs an async runtime: enable its `tokio` feature (on by default) or its `smol` feature"
);

pub(crate) mod net;

use std::future::Future;
use std::pin::{pin, Pin};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::future::{select, Either};

/// The crate's clock.
///
/// Tokio's whenever the `tokio` feature is on, which reads the std clock
/// outside a tokio runtime and tokio's paused clock inside a test that paused
/// it (`download_tests` and `upload_tests` time everything that way). A
/// smol-only build has no tokio clock, so it is std's there. Both have the
/// same API, apart from the conversion [`to_std`] covers.
#[cfg(feature = "tokio")]
pub(crate) type Instant = tokio::time::Instant;
/// The crate's clock. See the `tokio`-feature definition.
#[cfg(not(feature = "tokio"))]
pub(crate) type Instant = std::time::Instant;

/// `instant` as std's clock, for an API that wants one.
#[cfg(feature = "smol")]
fn to_std(instant: Instant) -> std::time::Instant {
    #[cfg(feature = "tokio")]
    {
        instant.into_std()
    }
    #[cfg(not(feature = "tokio"))]
    {
        instant
    }
}

/// Which runtime a call runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    /// A tokio runtime is in reach.
    #[cfg(feature = "tokio")]
    Tokio,
    /// Everything else: smol's reactor and global executor, which work
    /// under any executor, `smol::block_on` and `futures::executor` included.
    #[cfg(feature = "smol")]
    Smol,
}

/// What a tokio-only build says when it is called outside a tokio runtime.
/// Tokio's own panic would name tokio and nothing else, which sends a smol
/// user looking in the wrong place.
#[cfg(all(feature = "tokio", not(feature = "smol")))]
const NO_TOKIO_RUNTIME: &str = "smb2 was called outside a tokio runtime, and it was built with only \
     its `tokio` runtime feature. Run it inside a tokio runtime, or enable smb2's `smol` feature to \
     run it on smol or any other executor.";

/// The runtime the current call runs on. See the module docs.
///
/// # Panics
///
/// In a build with only the `tokio` feature, when there is no tokio runtime
/// in reach.
pub(crate) fn backend() -> Backend {
    #[cfg(all(feature = "tokio", feature = "smol"))]
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            Backend::Tokio
        } else {
            Backend::Smol
        }
    }
    #[cfg(all(feature = "tokio", not(feature = "smol")))]
    {
        if tokio::runtime::Handle::try_current().is_err() {
            panic!("{NO_TOKIO_RUNTIME}");
        }
        Backend::Tokio
    }
    #[cfg(all(feature = "smol", not(feature = "tokio")))]
    {
        Backend::Smol
    }
}

/// A spawned background task.
///
/// **Dropping the handle detaches the task; only [`abort`](Self::abort)
/// stops it.** That is tokio's `JoinHandle` contract, and every task in this
/// crate is written against it: the connection's plumbing is spawned, its
/// handle parked in `Inner`, and the task keeps running until `Inner::drop`
/// aborts it. smol's `Task` does the opposite (drop cancels), so the smol arm
/// detaches explicitly on drop. ❌ Don't hold a raw `smol::Task` anywhere in
/// the crate: a dropped one silently stops the writer or receiver task.
#[derive(Debug)]
#[must_use = "dropping a TaskHandle detaches the task; keep it to be able to abort it"]
pub(crate) struct TaskHandle(TaskInner);

#[derive(Debug)]
enum TaskInner {
    #[cfg(feature = "tokio")]
    Tokio(tokio::task::JoinHandle<()>),
    /// `None` once aborted, so `Drop` has nothing left to detach.
    #[cfg(feature = "smol")]
    Smol(Option<smol::Task<()>>),
}

impl TaskHandle {
    /// Let the task run to completion on its own, with nothing left to stop
    /// it. The same as dropping the handle; this says so at the call site.
    pub(crate) fn detach(self) {}

    /// Stop the task. Its future is dropped at its next await point (or right
    /// away, if it is not running), which is what releases whatever it holds.
    pub(crate) fn abort(mut self) {
        match &mut self.0 {
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(handle) => handle.abort(),
            // Dropping a smol task is how it is cancelled. Taking it leaves
            // `Drop` nothing to detach.
            #[cfg(feature = "smol")]
            TaskInner::Smol(task) => drop(task.take()),
        }
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        match &mut self.0 {
            // A tokio JoinHandle detaches on drop by itself.
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(_) => {}
            #[cfg(feature = "smol")]
            TaskInner::Smol(task) => {
                if let Some(task) = task.take() {
                    task.detach();
                }
            }
        }
    }
}

/// Run `future` in the background, on the current call's backend.
///
/// On smol that is smol's global executor, which runs on threads of its own
/// (`SMOL_THREADS`, one by default), so the task makes progress whatever
/// executor the caller is on.
pub(crate) fn spawn<F>(future: F) -> TaskHandle
where
    F: Future<Output = ()> + Send + 'static,
{
    match backend() {
        #[cfg(feature = "tokio")]
        Backend::Tokio => TaskHandle(TaskInner::Tokio(tokio::spawn(future))),
        #[cfg(feature = "smol")]
        Backend::Smol => TaskHandle(TaskInner::Smol(Some(smol::spawn(future)))),
    }
}

/// [`spawn`], or `None` where `spawn` would panic: a build with only the
/// `tokio` feature, called outside a tokio runtime. For `Drop`, which runs
/// wherever the owner happens to let go and must never panic.
pub(crate) fn try_spawn<F>(future: F) -> Option<TaskHandle>
where
    F: Future<Output = ()> + Send + 'static,
{
    #[cfg(all(feature = "tokio", not(feature = "smol")))]
    if tokio::runtime::Handle::try_current().is_err() {
        return None;
    }
    Some(spawn(future))
}

/// Give the executor one turn: the current task goes to the back of the
/// queue, and whatever is ready runs first.
///
/// On tokio this is what lets a task we just woke run: tokio puts it in this
/// worker's LIFO slot, which no other worker may steal, so it waits for us to
/// yield.
///
/// A self-wake, the same on every executor (it's what smol's `yield_now` is).
/// Tokio requeues a task that woke itself during its poll at the back of the
/// local queue, behind the LIFO slot, so it does the job there too. ❌ Don't
/// swap in `tokio::task::yield_now`: it defers the wake until the worker next
/// polls the I/O driver, which measured 5–13 µs a call against ~40 ns here,
/// and the receiver task pays it once per frame.
pub(crate) fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

/// The future [`yield_now`] returns.
#[derive(Debug)]
#[must_use = "a yield does nothing unless awaited"]
pub(crate) struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            return Poll::Ready(());
        }
        self.yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// A timer: resolves once its deadline has passed.
///
/// `Unpin`, so it can be raced with `futures_util::future::select` after a
/// plain `pin!`. The tokio arm is boxed because tokio's `Sleep` is not
/// `Unpin`; one allocation per timer is noise next to the frame each one
/// guards.
#[derive(Debug)]
#[must_use = "a timer does nothing unless awaited"]
pub(crate) struct Sleep(SleepInner);

#[derive(Debug)]
enum SleepInner {
    #[cfg(feature = "tokio")]
    Tokio(Pin<Box<tokio::time::Sleep>>),
    #[cfg(feature = "smol")]
    Smol(smol::Timer),
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match &mut self.get_mut().0 {
            #[cfg(feature = "tokio")]
            SleepInner::Tokio(sleep) => sleep.as_mut().poll(cx),
            #[cfg(feature = "smol")]
            SleepInner::Smol(timer) => Pin::new(timer).poll(cx).map(|_| ()),
        }
    }
}

/// A timer that fires `duration` from now.
pub(crate) fn sleep(duration: Duration) -> Sleep {
    sleep_until(deadline_after(duration))
}

/// A timer that fires at `deadline`.
pub(crate) fn sleep_until(deadline: Instant) -> Sleep {
    match backend() {
        #[cfg(feature = "tokio")]
        Backend::Tokio => Sleep(SleepInner::Tokio(Box::pin(tokio::time::sleep_until(
            deadline,
        )))),
        #[cfg(feature = "smol")]
        Backend::Smol => Sleep(SleepInner::Smol(smol::Timer::at(to_std(deadline)))),
    }
}

/// A [`timeout`] or [`timeout_at`] ran out before its future finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Elapsed;

/// Run `future`, giving up after `duration`.
///
/// The deadline is taken when this is CALLED, not when the result is first
/// polled, which is tokio's `timeout` contract. The future is polled before
/// the timer, so one that is already done wins even at a passed deadline.
pub(crate) fn timeout<F: Future>(
    duration: Duration,
    future: F,
) -> impl Future<Output = Result<F::Output, Elapsed>> {
    timeout_at(deadline_after(duration), future)
}

/// Run `future`, giving up at `deadline`. See [`timeout`].
pub(crate) async fn timeout_at<F: Future>(
    deadline: Instant,
    future: F,
) -> Result<F::Output, Elapsed> {
    let future = pin!(future);
    match select(future, sleep_until(deadline)).await {
        Either::Left((output, _)) => Ok(output),
        Either::Right(((), _)) => Err(Elapsed),
    }
}

/// `now + duration`, saturating to a deadline that never comes in practice.
/// Tokio does the same (about 30 years out) so `Duration::MAX` means "no
/// deadline" rather than an overflow panic.
fn deadline_after(duration: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(duration)
        .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365 * 30))
}

#[cfg(test)]
mod tests;
