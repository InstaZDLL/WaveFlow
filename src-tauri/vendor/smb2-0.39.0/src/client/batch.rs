//! Many small operations overlapped on one connection, handed over as each
//! finishes.
//!
//! The engine under the crate's pipelined multi-file calls. It knows nothing
//! about what an item does: `run` takes an item count and a closure making
//! one item's future, so that closure decides which `Connection` and `Tree` the
//! item runs on, and nothing here assumes there's only one.
//!
//! Three decisions, all from measuring the QNAP (`docs/notes/named-streams-and-xattrs.md`):
//!
//! - **Results arrive as items finish**, because `smbd` sometimes blocks for
//!   seconds on one open and everything queued behind it waits. A consumer
//!   painting a listing wants the fast 98% now. [`BatchResults::collect_in_order`]
//!   is the thin wrapper for everyone else.
//! - ❌ **No per-item timeout.** A stall is a working server being slow, and a
//!   tighter bound would report working files as failed on exactly that server.
//!   The connection's liveness handling already tells slow from dead.
//! - **The window is a count of items, bounded by credits** (`window_for`),
//!   and an item that can't get a credit parks in `reserve_credits`, so a small
//!   window slows the batch down and never fails it.

use std::future::Future;
use std::pin::Pin;

use futures_util::stream::{self, Stream, StreamExt};

use crate::client::connection::Connection;
use crate::error::Result;

/// Items in flight when the caller doesn't say. `smbd` is single-threaded per
/// connection, so past this the QNAP's throughput stays flat and each item only
/// waits longer: 13 ms at 16 and 52 ms at 64, on top of everything else on the
/// connection (`docs/notes/named-streams-and-xattrs.md`).
const DEFAULT_WINDOW: usize = 16;

/// How a pipelined multi-file call runs.
///
/// Start from [`new`](Self::new), which is the safe answer for any path list.
///
/// ```
/// let options = smb2::BatchOptions::new().assume_paths_exist(true);
/// # let _ = options;
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct BatchOptions {
    pub(crate) assume_paths_exist: bool,
    pub(crate) window: Option<usize>,
}

impl BatchOptions {
    /// The defaults: every "not found" is checked, and the crate sizes the
    /// window.
    pub fn new() -> Self {
        Self::default()
    }

    /// The caller just listed these paths, so treat "not found" as "no such
    /// attribute" without asking whether the file is still there.
    ///
    /// Every file then costs one round trip, however many carry the attribute.
    /// Without this, a file carrying it costs two, and one without it costs one
    /// that's a bit cheaper on the server. The price: a file deleted since the
    /// listing reads as `Ok(None)` rather than
    /// [`ErrorKind::NotFound`](crate::ErrorKind::NotFound).
    pub fn assume_paths_exist(mut self, assume: bool) -> Self {
        self.assume_paths_exist = assume;
        self
    }

    /// Keep at most this many items in flight, instead of what the crate
    /// picks. The credit window still bounds it, and 0 counts as 1.
    pub fn window(mut self, window: usize) -> Self {
        self.window = Some(window.max(1));
        self
    }
}

/// How many items to keep in flight on `conn`, each charging `charge` credits.
///
/// The crate-wide rule: what it sizes itself fits in half the credit ceiling
/// once the server has shown one, because a watcher's long poll on the same
/// connection holds credits that aren't coming back on cue. Always at least 1.
pub(crate) fn window_for(conn: &Connection, charge: u16, requested: Option<usize>) -> usize {
    let wanted = requested.unwrap_or(DEFAULT_WINDOW).max(1);
    match conn.comfortable_charge() {
        Some(budget) => wanted.min(usize::from(budget / charge.max(1)).max(1)),
        None => wanted,
    }
}

/// The results of a pipelined multi-file call, as each item finishes.
///
/// Each result comes with the index of its item in the input. Nothing goes on
/// the wire unless this is being polled: drive it to the end, or drop it to stop.
/// Dropping it sends no more items and leaves the answers to what's already
/// sent to be discarded. [`next`](Self::next) is cancel-safe, so it can sit in a
/// `select!`.
pub struct BatchResults<T> {
    inner: Pin<Box<dyn Stream<Item = (usize, Result<T>)> + Send>>,
    count: usize,
}

impl<T> std::fmt::Debug for BatchResults<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchResults")
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl<T> BatchResults<T> {
    /// The next item to finish and its index in the input, or `None` when all
    /// of them have.
    pub async fn next(&mut self) -> Option<(usize, Result<T>)> {
        self.inner.next().await
    }

    /// Wait for every item, and put the results in input order.
    pub async fn collect_in_order(mut self) -> Vec<Result<T>> {
        let mut slots: Vec<Option<Result<T>>> = (0..self.count).map(|_| None).collect();
        while let Some((index, result)) = self.next().await {
            slots[index] = Some(result);
        }
        slots
            .into_iter()
            .map(|slot| slot.expect("the batch answers every index exactly once"))
            .collect()
    }
}

/// Run `count` items with at most `window` in flight.
pub(crate) fn run<T, F, Fut>(count: usize, window: usize, mut item: F) -> BatchResults<T>
where
    T: Send + 'static,
    F: FnMut(usize) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>> + Send + 'static,
{
    let inner = stream::iter(0..count)
        .map(move |index| {
            let fut = item(index);
            async move { (index, fut.await) }
        })
        .buffer_unordered(window.max(1));
    BatchResults {
        inner: Box::pin(inner),
        count,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::Error;

    #[tokio::test]
    async fn results_keep_the_input_order_when_collected() {
        let results = run(5, 2, |index| async move { Ok(index * 10) })
            .collect_in_order()
            .await;
        let values: Vec<usize> = results.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(values, [0, 10, 20, 30, 40]);
    }

    #[tokio::test]
    async fn one_failure_stays_with_its_item() {
        let results = run(3, 3, |index| async move {
            if index == 1 {
                Err(Error::invalid_data("this one"))
            } else {
                Ok(index)
            }
        })
        .collect_in_order()
        .await;
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert!(results[2].is_ok());
    }

    /// The point of the runner: an item stuck behind a slow server doesn't hold
    /// back what's already done.
    #[tokio::test]
    async fn a_stalled_item_doesnt_hold_back_the_finished_ones() {
        let (release, stalled) = tokio::sync::oneshot::channel::<()>();
        let stalled = Arc::new(std::sync::Mutex::new(Some(stalled)));
        let mut results = run(3, 3, move |index| {
            let stalled = stalled.lock().unwrap().take();
            async move {
                if index == 0 {
                    stalled.unwrap().await.unwrap();
                }
                Ok(index)
            }
        });
        let first = results.next().await.unwrap();
        let second = results.next().await.unwrap();
        assert_eq!([first.0, second.0], [1, 2]);
        release.send(()).unwrap();
        assert_eq!(results.next().await.unwrap().0, 0);
        assert!(results.next().await.is_none());
    }

    #[tokio::test]
    async fn never_more_than_the_window_in_flight() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (i, p) = (in_flight.clone(), peak.clone());
        let results = run(50, 4, move |_| {
            let (i, p) = (i.clone(), p.clone());
            async move {
                let now = i.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                i.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .collect_in_order()
        .await;
        assert_eq!(results.len(), 50);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn an_empty_batch_finishes_at_once() {
        let results = run(0, 8, |_| async { Ok(()) }).collect_in_order().await;
        assert!(results.is_empty());
    }
}
