//! Live byte counts for what a transport is receiving.
//!
//! A transport hands the connection a frame only once the whole of it has
//! arrived, and a large one can spend a long time arriving: an 8 MB READ
//! response over a 200 KB/s link is 40 s on the wire. [`ReceiveProgress`] is
//! how the bytes inside it become visible while that happens, to a rate
//! display and, more importantly, to the connection's liveness clock.
//!
//! **Counts and timestamps only, never data.** A partial frame is unverified:
//! a signature (or an AEAD tag, when the session encrypts) covers the whole
//! message, so nothing may act on its bytes until the frame is complete and
//! has passed verification. Knowing that bytes are landing is safe; knowing
//! what they say is not.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// "No frame is arriving", in [`ReceiveProgress::frame_len`].
const NO_FRAME: u64 = u64::MAX;

/// Byte counts for what a transport is receiving, published as the bytes land
/// rather than when a frame completes, and readable from any thread.
///
/// A transport owns one, updates it from its receive path, and hands it out
/// through [`TransportReceive::receive_progress`](super::TransportReceive::receive_progress).
/// The update side is built for the hot path: [`record`](Self::record) is one
/// relaxed add, one clock read, and one relaxed store, with no allocation and
/// no lock.
///
/// Counts **payload** bytes, the SMB2 message(s) inside each frame, and not
/// the 4-byte length prefix, so once a frame completes the running total
/// agrees with the connection's `wire_bytes_received` counter.
///
/// The fields are separate atomics, so a [`snapshot`](Self::snapshot) is
/// eventually consistent like every other diagnostic in this crate: `frame`
/// can trail `bytes` by one update. The snapshot clamps what it reports so a
/// torn read can never show a frame more than complete.
#[derive(Debug)]
pub struct ReceiveProgress {
    /// What `last_byte_at` counts from.
    epoch: Instant,
    /// Payload bytes received, every frame, the one still arriving included.
    bytes: AtomicU64,
    /// Nanoseconds from `epoch` to the last byte, plus one so zero can mean
    /// "never".
    last_byte_at: AtomicU64,
    /// `bytes` as it stood when the current frame's prefix arrived.
    frame_start: AtomicU64,
    /// The current frame's payload length, or [`NO_FRAME`] between frames.
    /// Published last on the way in and first on the way out, so a reader that
    /// sees a length also sees the `frame_start` that goes with it.
    frame_len: AtomicU64,
}

impl ReceiveProgress {
    /// A counter that has seen nothing yet.
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            bytes: AtomicU64::new(0),
            last_byte_at: AtomicU64::new(0),
            frame_start: AtomicU64::new(0),
            frame_len: AtomicU64::new(NO_FRAME),
        }
    }

    /// A frame's length prefix has arrived, and `len` payload bytes follow.
    ///
    /// The prefix is bytes from the server too, so this stamps the clock.
    pub fn begin_frame(&self, len: usize) {
        self.stamp();
        self.frame_start
            .store(self.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
        self.frame_len.store(len as u64, Ordering::Release);
    }

    /// `n` more payload bytes of the current frame have landed.
    pub fn record(&self, n: usize) {
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        self.stamp();
    }

    /// The current frame is over: complete, or abandoned because the
    /// transport failed partway.
    pub fn end_frame(&self) {
        self.frame_len.store(NO_FRAME, Ordering::Release);
    }

    /// A whole frame of `len` payload bytes arrived at once.
    ///
    /// For a transport that only ever sees complete frames. Keeps the totals
    /// and the clock right, and can say nothing about a frame in progress,
    /// because there never is one it can see.
    pub fn record_whole_frame(&self, len: usize) {
        self.begin_frame(len);
        self.record(len);
        self.end_frame();
    }

    /// What has arrived so far.
    pub fn snapshot(&self) -> ReceiveSnapshot {
        // Acquire pairs with `begin_frame`'s release, so a length read here
        // comes with the `frame_start` stored before it, and `bytes` read
        // after that can't be older than the start.
        let len = self.frame_len.load(Ordering::Acquire);
        let start = self.frame_start.load(Ordering::Relaxed);
        let bytes = self.bytes.load(Ordering::Relaxed);
        let frame = (len != NO_FRAME).then(|| FrameProgress {
            received: bytes.saturating_sub(start).min(len),
            len,
        });
        let last_byte_at = match self.last_byte_at.load(Ordering::Relaxed) {
            0 => None,
            nanos => self.epoch.checked_add(Duration::from_nanos(nanos - 1)),
        };
        ReceiveSnapshot {
            bytes,
            frame,
            last_byte_at,
        }
    }

    fn stamp(&self) {
        let nanos = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX - 1);
        self.last_byte_at.store(nanos + 1, Ordering::Relaxed);
    }
}

impl Default for ReceiveProgress {
    fn default() -> Self {
        Self::new()
    }
}

/// A reading of a [`ReceiveProgress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReceiveSnapshot {
    /// Payload bytes received so far, including the part of a frame that is
    /// still arriving.
    pub bytes: u64,
    /// The frame currently arriving, or `None` between frames.
    pub frame: Option<FrameProgress>,
    /// When the last byte from the server landed, or `None` if none has.
    pub last_byte_at: Option<Instant>,
}

/// How far into a frame the transport is.
///
/// ❌ Never treat `received` as data you have. The frame is unverified until
/// it completes, so this is good for a rate or a liveness signal, and a byte
/// bar that counted it would have to roll back if the frame then failed its
/// signature check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct FrameProgress {
    /// Payload bytes of this frame received so far.
    pub received: u64,
    /// The frame's payload length, from its length prefix.
    pub len: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_counter_has_seen_nothing() {
        let p = ReceiveProgress::new();
        let s = p.snapshot();
        assert_eq!(s.bytes, 0);
        assert_eq!(s.frame, None);
        assert_eq!(s.last_byte_at, None);
    }

    #[test]
    fn a_frame_in_progress_reports_how_far_in_it_is() {
        let p = ReceiveProgress::new();
        p.record_whole_frame(100);
        p.begin_frame(1000);
        p.record(300);
        let s = p.snapshot();
        assert_eq!(s.bytes, 400, "the running total includes the partial frame");
        assert_eq!(
            s.frame,
            Some(FrameProgress {
                received: 300,
                len: 1000
            }),
            "the frame counts from its own start, not from the connection's"
        );
        assert!(s.last_byte_at.is_some());

        p.record(700);
        p.end_frame();
        let s = p.snapshot();
        assert_eq!(s.bytes, 1100);
        assert_eq!(s.frame, None);
    }

    #[test]
    fn the_clock_moves_with_every_piece() {
        let p = ReceiveProgress::new();
        p.begin_frame(10);
        let first = p.snapshot().last_byte_at.unwrap();
        std::thread::sleep(Duration::from_millis(5));
        p.record(1);
        let second = p.snapshot().last_byte_at.unwrap();
        assert!(second > first, "a piece landing is a sign of life");
    }

    #[test]
    fn an_empty_frame_still_counts_as_the_server_speaking() {
        let p = ReceiveProgress::new();
        p.record_whole_frame(0);
        let s = p.snapshot();
        assert_eq!(s.bytes, 0);
        assert!(s.last_byte_at.is_some());
    }
}
