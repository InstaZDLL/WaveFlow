//! Mock transport for testing.
//!
//! Provides a [`MockTransport`] that queues canned responses and records
//! sent messages, enabling test-driven development of higher layers
//! without needing a real SMB server.

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::error::{Error, Result};
use crate::transport::{ReceiveProgress, TransportReceive, TransportSend};

/// A mock transport that queues responses and records sent messages.
///
/// Use this in tests to simulate server conversations without a real
/// network connection. Responses are returned in FIFO order.
///
/// `receive()` awaits on an internal `Notify` when the queue is empty,
/// so the background receiver task doesn't exit prematurely between
/// `queue_response` calls. Explicit disconnect is triggered by calling
/// [`Self::close`].
pub struct MockTransport {
    /// Responses to return on `receive()`, in order.
    responses: Mutex<VecDeque<Vec<u8>>>,
    /// Messages that were sent, for assertions.
    sent: Mutex<Vec<Vec<u8>>>,
    /// How many times `receive()` was called successfully (returning Ok).
    receive_count: Mutex<usize>,
    /// Wakes receivers when a response is queued or `close()` is called.
    notify: Notify,
    /// Set by `close()` to signal end-of-stream.
    closed: AtomicBool,
    /// When `true`, `receive()` rewrites each response sub-frame's
    /// `MessageId` to match the `MessageId` of the next pending sent request
    /// (and consumes it). See [`Self::enable_auto_rewrite_msg_id`].
    auto_rewrite: AtomicBool,
    /// FIFO of `MessageId`s observed in `send()` that haven't yet been
    /// consumed by a `receive()` rewrite. Only used when `auto_rewrite`
    /// is on.
    pending_sent_msg_ids: Mutex<VecDeque<u64>>,
    /// Signaled whenever a new send is recorded or a close happens — used
    /// by `receive()` in auto-rewrite mode to wait for a sent msg_id to
    /// pair with a queued response.
    send_notify: Notify,
    /// Counts every response handed out, as one whole frame each: a queued
    /// response has no network under it to arrive gradually.
    progress: Arc<ReceiveProgress>,
}

impl MockTransport {
    /// Create a new mock with no queued responses.
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            receive_count: Mutex::new(0),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
            auto_rewrite: AtomicBool::new(false),
            pending_sent_msg_ids: Mutex::new(VecDeque::new()),
            send_notify: Notify::new(),
            progress: Arc::new(ReceiveProgress::new()),
        }
    }

    /// Enable msg_id rewriting: when `true`, `receive()` rewrites each
    /// response sub-frame's `MessageId` in-place to match the `MessageId`
    /// of the next request recorded by `send()` (FIFO pairing).
    ///
    /// Without this, canned response builders hardcode `MessageId(0)` and
    /// won't match the caller's allocated msg_ids — the receiver task
    /// drops them as orphans and every caller hangs. This mode is the
    /// test-fixture replacement for the pre-Phase-3 orphan-filter-off
    /// path. Compound responses (multiple sub-frames chained via
    /// `NextCommand`) each consume one sent msg_id in order.
    ///
    /// The receive side blocks until both a queued response and a sent
    /// msg_id are available, so tests can queue responses before or
    /// after the caller sends.
    pub fn enable_auto_rewrite_msg_id(&self) {
        self.auto_rewrite.store(true, Ordering::Release);
    }

    /// Queue a response to be returned by the next `receive()` call.
    pub fn queue_response(&self, data: Vec<u8>) {
        self.responses.lock().unwrap().push_back(data);
        self.notify.notify_one();
    }

    /// Queue multiple responses to be returned in order.
    pub fn queue_responses(&self, responses: Vec<Vec<u8>>) {
        let mut guard = self.responses.lock().unwrap();
        let count = responses.len();
        for r in responses {
            guard.push_back(r);
        }
        drop(guard);
        for _ in 0..count {
            self.notify.notify_one();
        }
    }

    /// Signal end-of-stream: after all queued responses are drained,
    /// `receive()` returns `Err(Error::Disconnected)`.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        // Use `notify_one` (stores a permit for the next `notified().await`)
        // in addition to `notify_waiters` (wakes currently-parked waiters).
        // `notify_waiters` alone loses the signal if `close()` fires
        // between `receive()`'s `closed.load()` check and its
        // `notified().await` — no waiter is parked yet, so nothing gets
        // woken. The stored permit from `notify_one` covers that gap.
        self.notify.notify_one();
        self.notify.notify_waiters();
        // Same treatment for the send-notification used by auto-rewrite:
        // close should wake a receive that's blocked waiting for a paired
        // sent msg_id so it observes `closed` and bails out.
        self.send_notify.notify_one();
        self.send_notify.notify_waiters();
    }

    /// Get all messages that were sent.
    pub fn sent_messages(&self) -> Vec<Vec<u8>> {
        self.sent.lock().unwrap().clone()
    }

    /// Get the nth sent message, or `None` if out of bounds.
    pub fn sent_message(&self, n: usize) -> Option<Vec<u8>> {
        self.sent.lock().unwrap().get(n).cloned()
    }

    /// How many messages have been sent.
    pub fn sent_count(&self) -> usize {
        self.sent.lock().unwrap().len()
    }

    /// Clear all recorded sent messages.
    pub fn clear_sent(&self) {
        self.sent.lock().unwrap().clear();
    }

    /// How many times `receive()` was called successfully (returned Ok).
    pub fn received_count(&self) -> usize {
        *self.receive_count.lock().unwrap()
    }

    /// How many responses are still queued and unread.
    ///
    /// Useful in tests that want to assert the code-under-test consumed
    /// every response it was expected to, without leaking any to a
    /// later test or leaving stale state that could mask a bug.
    pub fn pending_responses(&self) -> usize {
        self.responses.lock().unwrap().len()
    }

    /// Assert that every queued response has been consumed.
    ///
    /// Panics with a descriptive message if any responses remain in the
    /// queue. Use at the end of a test to catch the "caller forgot to
    /// receive" pattern that produces response-pipe pollution in
    /// real usage.
    #[track_caller]
    pub fn assert_fully_consumed(&self) {
        let remaining = self.pending_responses();
        assert_eq!(
            remaining, 0,
            "MockTransport has {} queued response(s) the code-under-test never read. \
             This usually means a caller sent a request but never received its response, \
             which in real usage leaves an orphan on the wire and corrupts the next op.",
            remaining
        );
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TransportSend for MockTransport {
    async fn send(&self, data: &[u8]) -> Result<()> {
        // In auto-rewrite mode, capture the MessageId of each sub-frame
        // so `receive()` can rewrite a queued response to match.
        if self.auto_rewrite.load(Ordering::Acquire) {
            for msg_id in extract_msg_ids(data) {
                self.pending_sent_msg_ids.lock().unwrap().push_back(msg_id);
                self.send_notify.notify_one();
            }
        }
        self.sent.lock().unwrap().push(data.to_vec());
        Ok(())
    }
}

#[async_trait]
impl TransportReceive for MockTransport {
    async fn receive(&self) -> Result<Vec<u8>> {
        loop {
            let auto = self.auto_rewrite.load(Ordering::Acquire);
            // Wait for a queued response first (auto mode and plain mode
            // both need one to exist).
            let has_response = !self.responses.lock().unwrap().is_empty();
            if !has_response {
                if self.closed.load(Ordering::Acquire) {
                    return Err(Error::Disconnected);
                }
                self.notify.notified().await;
                continue;
            }

            if auto {
                // We have a response; peek its sub-frame count and wait
                // for at least that many sent msg_ids to be queued
                // (one consumed per sub-frame, even ones that already
                // have non-zero msg_ids, so pairing stays 1:1).
                let needed = {
                    let guard = self.responses.lock().unwrap();
                    match guard.front() {
                        Some(frame) => count_sub_frames(frame),
                        None => continue,
                    }
                };
                if needed > 0 {
                    loop {
                        let have = self.pending_sent_msg_ids.lock().unwrap().len();
                        if have >= needed {
                            break;
                        }
                        if self.closed.load(Ordering::Acquire) {
                            return Err(Error::Disconnected);
                        }
                        self.send_notify.notified().await;
                    }
                }
                // Consume one response and `needed` sent msg_ids,
                // rewriting each sub-frame's zero msg_id to match the
                // corresponding sent msg_id.
                let mut data = match self.responses.lock().unwrap().pop_front() {
                    Some(d) => d,
                    None => continue,
                };
                let mut ids = self.pending_sent_msg_ids.lock().unwrap();
                rewrite_msg_ids(&mut data, &mut ids);
                drop(ids);
                *self.receive_count.lock().unwrap() += 1;
                self.progress.record_whole_frame(data.len());
                return Ok(data);
            }

            // Plain mode: just pop and return.
            let data = match self.responses.lock().unwrap().pop_front() {
                Some(d) => d,
                None => continue,
            };
            *self.receive_count.lock().unwrap() += 1;
            self.progress.record_whole_frame(data.len());
            return Ok(data);
        }
    }

    fn receive_progress(&self) -> Option<Arc<ReceiveProgress>> {
        Some(Arc::clone(&self.progress))
    }
}

/// Extract `MessageId`s from a packed SMB2 request frame (possibly compound).
/// Returns one msg_id per sub-frame, following `NextCommand` offsets.
/// Returns an empty Vec if the data isn't a recognizable SMB2 frame —
/// e.g. when `send()` is used with arbitrary bytes in transport-level tests.
fn extract_msg_ids(data: &[u8]) -> Vec<u64> {
    const HEADER_MIN: usize = 64;
    if data.len() < HEADER_MIN {
        return Vec::new();
    }
    // Not an SMB2 header — skip (non-SMB2 tests call send with arbitrary bytes).
    if &data[0..4] != b"\xFESMB" {
        return Vec::new();
    }
    let mut ids = Vec::new();
    let mut offset = 0usize;
    loop {
        if offset + HEADER_MIN > data.len() {
            break;
        }
        let msg_id =
            u64::from_le_bytes(data[offset + 24..offset + 32].try_into().unwrap_or([0; 8]));
        ids.push(msg_id);
        let next = u32::from_le_bytes(data[offset + 20..offset + 24].try_into().unwrap_or([0; 4]));
        if next == 0 {
            break;
        }
        offset += next as usize;
    }
    ids
}

/// Count sub-frames in a packed SMB2 response frame by walking
/// `NextCommand` offsets. Returns 0 for non-SMB2 frames, otherwise the
/// total sub-frame count. `rewrite_msg_ids` consumes one sent msg_id
/// per sub-frame (even those with already-set msg_ids) to keep
/// send→receive pairing strictly 1:1 and avoid queue drift in tests
/// that hardcode some but not all msg_ids.
fn count_sub_frames(data: &[u8]) -> usize {
    const HEADER_MIN: usize = 64;
    if data.len() < HEADER_MIN || &data[0..4] != b"\xFESMB" {
        return 0;
    }
    let mut count = 0usize;
    let mut offset = 0usize;
    loop {
        if offset + HEADER_MIN > data.len() {
            break;
        }
        count += 1;
        let next = u32::from_le_bytes(data[offset + 20..offset + 24].try_into().unwrap_or([0; 4]));
        if next == 0 {
            break;
        }
        offset += next as usize;
    }
    count
}

/// Rewrite each sub-frame's `MessageId` in-place, consuming one id from
/// `ids` per sub-frame in FIFO order. Sub-frames whose msg_id is
/// already non-zero keep their hardcoded id (so tests exercising out-of-
/// order routing still work) but STILL consume one id from the queue
/// to keep send→receive pairing 1:1.
fn rewrite_msg_ids(data: &mut [u8], ids: &mut VecDeque<u64>) {
    const HEADER_MIN: usize = 64;
    if data.len() < HEADER_MIN || &data[0..4] != b"\xFESMB" {
        return;
    }
    let mut offset = 0usize;
    loop {
        if offset + HEADER_MIN > data.len() {
            break;
        }
        let existing =
            u64::from_le_bytes(data[offset + 24..offset + 32].try_into().unwrap_or([0; 8]));
        let consumed = ids.pop_front();
        if existing == 0 {
            if let Some(id) = consumed {
                data[offset + 24..offset + 32].copy_from_slice(&id.to_le_bytes());
            } else {
                break;
            }
        }
        let next = u32::from_le_bytes(data[offset + 20..offset + 24].try_into().unwrap_or([0; 4]));
        if next == 0 {
            break;
        }
        offset += next as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queue_response_and_receive_it() {
        let mock = MockTransport::new();
        let data = vec![0x01, 0x02, 0x03];
        mock.queue_response(data.clone());

        let received = mock.receive().await.unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn queue_multiple_responses_received_in_order() {
        let mock = MockTransport::new();
        mock.queue_responses(vec![vec![0x01], vec![0x02, 0x03], vec![0x04, 0x05, 0x06]]);

        assert_eq!(mock.receive().await.unwrap(), vec![0x01]);
        assert_eq!(mock.receive().await.unwrap(), vec![0x02, 0x03]);
        assert_eq!(mock.receive().await.unwrap(), vec![0x04, 0x05, 0x06]);
    }

    #[tokio::test]
    async fn close_causes_receive_to_return_disconnected() {
        let mock = MockTransport::new();
        mock.close();

        let result = mock.receive().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Disconnected),
            "expected Disconnected, got: {err}"
        );
    }

    #[tokio::test]
    async fn send_records_message() {
        let mock = MockTransport::new();
        let msg = vec![0xAA, 0xBB, 0xCC];

        mock.send(&msg).await.unwrap();

        let sent = mock.sent_messages();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], msg);
    }

    #[tokio::test]
    async fn sent_count_tracks_correctly() {
        let mock = MockTransport::new();
        assert_eq!(mock.sent_count(), 0);

        mock.send(&[0x01]).await.unwrap();
        assert_eq!(mock.sent_count(), 1);

        mock.send(&[0x02]).await.unwrap();
        assert_eq!(mock.sent_count(), 2);

        mock.send(&[0x03]).await.unwrap();
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn sent_message_returns_nth() {
        let mock = MockTransport::new();
        mock.send(&[0x0A]).await.unwrap();
        mock.send(&[0x0B]).await.unwrap();
        mock.send(&[0x0C]).await.unwrap();

        assert_eq!(mock.sent_message(0), Some(vec![0x0A]));
        assert_eq!(mock.sent_message(1), Some(vec![0x0B]));
        assert_eq!(mock.sent_message(2), Some(vec![0x0C]));
        assert_eq!(mock.sent_message(3), None);
    }

    #[tokio::test]
    async fn clear_sent_removes_all_recorded_messages() {
        let mock = MockTransport::new();
        mock.send(&[0x01]).await.unwrap();
        mock.send(&[0x02]).await.unwrap();
        assert_eq!(mock.sent_count(), 2);

        mock.clear_sent();
        assert_eq!(mock.sent_count(), 0);
        assert!(mock.sent_messages().is_empty());
    }

    #[tokio::test]
    async fn interleaved_send_and_receive() {
        let mock = MockTransport::new();
        mock.queue_responses(vec![vec![0xF1], vec![0xF2], vec![0xF3]]);

        // Send a request, receive a response, repeat.
        mock.send(&[0x01]).await.unwrap();
        assert_eq!(mock.receive().await.unwrap(), vec![0xF1]);

        mock.send(&[0x02]).await.unwrap();
        assert_eq!(mock.receive().await.unwrap(), vec![0xF2]);

        mock.send(&[0x03]).await.unwrap();
        assert_eq!(mock.receive().await.unwrap(), vec![0xF3]);

        // No more responses. Close to cause Disconnected.
        mock.close();
        assert!(mock.receive().await.is_err());

        // All three sends recorded.
        assert_eq!(mock.sent_count(), 3);
    }

    #[tokio::test]
    async fn concurrent_send_and_receive() {
        use std::sync::Arc;

        let mock = Arc::new(MockTransport::new());
        mock.queue_responses(vec![vec![0xAA]; 10]);

        let send_mock = Arc::clone(&mock);
        let send_task = tokio::spawn(async move {
            for i in 0..10u8 {
                send_mock.send(&[i]).await.unwrap();
            }
        });

        let recv_mock = Arc::clone(&mock);
        let recv_task = tokio::spawn(async move {
            let mut received = Vec::new();
            for _ in 0..10 {
                received.push(recv_mock.receive().await.unwrap());
            }
            received
        });

        send_task.await.unwrap();
        let received = recv_task.await.unwrap();

        assert_eq!(received.len(), 10);
        assert_eq!(mock.sent_count(), 10);
    }

    #[tokio::test]
    async fn empty_message_can_be_sent_and_received() {
        let mock = MockTransport::new();
        mock.queue_response(vec![]);

        mock.send(&[]).await.unwrap();
        let received = mock.receive().await.unwrap();

        assert!(received.is_empty());
        assert_eq!(mock.sent_message(0), Some(vec![]));
    }
}
