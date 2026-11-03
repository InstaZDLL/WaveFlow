//! How much an upload keeps on the wire.
//!
//! [`WriteBehind`] is the public knob. Every pipelined upload paces through
//! one engine (`write_pipe.rs`), which consults the same `Window` a download
//! does (`read_ahead.rs`): the arithmetic doesn't care which way the payload
//! flows, only when a request went out and when its bytes had arrived.
//!
//! # What a WRITE's answer measures
//!
//! A READ's answer carries the payload, so its arrival is the delivery. A
//! WRITE's answer is small and comes back after the payload went the other
//! way, so its arrival is the moment the server had every byte, plus the
//! server's own write and half a round trip. The gap between two answers is
//! the uplink's time for the second WRITE's bytes whenever that WRITE was
//! already queued behind the first, which is the same test the window applies
//! to READs (see `Window` § Measuring the link), so the arithmetic carries
//! over unchanged. Each answer is timed as the receiver task read it off the
//! wire, not when the upload looks at it: uploads are push-based, and a
//! producer busy elsewhere may not poll its answers for seconds.
//!
//! What can still make it read low, never high: **the answer shares the
//! downlink.** A download on the same connection queues the WRITE answers
//! behind its READ payload, and a slow server disk holds them back. Both look
//! like a slower uplink. Reading low keeps the window small, which errs toward
//! keeping the connection responsive.
//!
//! The rate is kept apart from the download one: an asymmetric link (most
//! home connections) moves each way at its own speed. So is the learned
//! headroom (`Window` § Learning the headroom): a confirmation that comes late
//! means the uplink sat idle or the server's disk held it, and either way the
//! window keeps more queued to cover the next one. A disk that stalls under
//! writes is exactly what that is for. On a 30 MB/s uplink it halves what a
//! `stat` waits behind an upload (70 ms against 142 ms, same throughput,
//! `results/self-tuning.md`), and at 3 MB/s / +60 ms a `stat` waits 161 ms
//! where 0.25.1's waited 343. An upload corrects what it estimates is still on
//! its way against the WRITEs actually unconfirmed, on every confirmation
//! (`Correction::Throughout`; `Window` § Closing the loop): without that, the
//! first WRITEs after an idle spell, crossing in TCP slow start, left a
//! surplus queued for the rest of the file (1.5 MiB, a `stat` waiting
//! ~520 ms). A download does the same only while its first flight lands.
//!
//! # Why adaptive is the default
//!
//! The same two constraints as for downloads (see [`crate::client::read_ahead`]):
//! a fast link needs bytes in flight, and on a slow one every queued byte is
//! latency for everything else on the connection. Before 0.25 a writer kept up
//! to 32 WRITEs of `MaxWriteSize` in flight (bounded at 32 MiB per connection
//! by the write budget), so a `stat` on the same connection waited behind all
//! of it: 23 s on a 375 KB/s uplink behind one 8 MiB file, and a Cmdr user's
//! 1 MiB frames sat 5–12 s in the send queue with every directory listing
//! behind them. The adaptive window keeps about `rate × (RTT + headroom)` in
//! flight instead: at least one WRITE, at most
//! [`ADAPTIVE_MAX_IN_FLIGHT`](crate::client::read_ahead::ADAPTIVE_MAX_IN_FLIGHT).
//!
//! Measured against Samba with the uplink shaped (`benchmarks/read-ahead/`,
//! `results/adaptive-uploads.md`, 2026-09-23), an 8 MiB upload against 0.24,
//! with the headroom then fixed at 250 ms:
//!
//! - **375 KB/s at +60 ms**: a `stat` on the same connection waits 1.5 s at
//!   most, down from 23.4 s, and a cancel is answered in 2.0 s, down from
//!   6.1 s. Same wall time.
//! - **3 MB/s at +20 ms**: a `stat` waits 370 ms at most, down from 2.9 s.
//! - **Fast links** keep their throughput: 100 MiB unshaped at 793 MB/s
//!   (0.24: 637), at +60 ms 45.6 MB/s (49.7), at +200 ms 16.0 MB/s (16.6),
//!   while the worst `stat` at +200 ms drops from 1.8 s to 416 ms. Doubling the
//!   cap to 8 MiB bought 2.5% at +200 ms for 50% more queue.

use crate::client::read_ahead::{quick_limit, Pacing};

/// Chunk size a [`FileWriter`](crate::FileWriter) writes by default, and the
/// one every pipelined upload uses: one WRITE's worth, capped at the server's
/// `MaxWriteSize`.
///
/// The same trade as [`DOWNLOAD_CHUNK_SIZE`](crate::DOWNLOAD_CHUNK_SIZE): small
/// enough that a slow uplink carries one in about a second and a half (at
/// 375 KB/s, where an 8 MiB WRITE took 22 s with nothing else moving), large
/// enough that a fast link needs few of them.
pub const UPLOAD_CHUNK_SIZE: u32 = 512 * 1024;

/// How many WRITEs an upload keeps on the wire.
///
/// Every pipelined upload paces this way: [`FileWriter`](crate::FileWriter),
/// [`FileUpload`](crate::FileUpload), [`Tree::write_file_pipelined`](crate::Tree::write_file_pipelined),
/// [`Tree::write_file_streamed`](crate::Tree::write_file_streamed), and
/// [`SmbClient::write_file_with_progress`](crate::SmbClient::write_file_with_progress).
/// A `FileWriter` takes the knob through
/// [`with_write_behind`](crate::FileWriter::with_write_behind); the others
/// use the default.
///
/// # Example
///
/// ```no_run
/// # async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
/// use smb2::WriteBehind;
///
/// // The behavior before 0.25: `MaxWriteSize` WRITEs, 32 in flight.
/// let max_write = client.params().map(|p| p.max_write_size).unwrap_or(65536);
/// let mut writer = client
///     .create_file_writer(share, "big.bin")
///     .await?
///     .with_chunk_size(max_write)
///     .with_write_behind(WriteBehind::Fixed(32));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WriteBehind {
    /// Size the window to the link (the default).
    ///
    /// Keeps about `uplink rate × (RTT + headroom)` bytes handed to the
    /// connection but not yet confirmed by the server: never fewer than one
    /// WRITE, never more than
    /// [`ADAPTIVE_MAX_IN_FLIGHT`](crate::client::read_ahead::ADAPTIVE_MAX_IN_FLIGHT).
    /// The uplink rate is re-measured over the last eight confirmations that
    /// queued behind each other; the RTT is the connection's NEGOTIATE round
    /// trip or the fastest WRITE so far, whichever is smaller; the headroom
    /// is learned per connection from how late confirmations come. See the
    /// [module docs](crate::client::write_behind).
    #[default]
    Adaptive,
    /// Keep exactly this many WRITEs in flight (values below one mean one).
    ///
    /// A big fixed window is fastest on a fast link and worst on a slow one:
    /// every other request on the connection queues behind all of it.
    Fixed(usize),
}

impl WriteBehind {
    /// One WRITE at a time.
    pub const SEQUENTIAL: Self = Self::Fixed(1);
}

impl From<WriteBehind> for Pacing {
    fn from(policy: WriteBehind) -> Self {
        match policy {
            WriteBehind::Adaptive => Self::Adaptive,
            WriteBehind::Fixed(n) => Self::Fixed(n),
        }
    }
}

/// The largest write worth sending as one frame: what the uplink moves in
/// `QUICK_FRAME_BUDGET` at `rate` bytes/s, never less than one upload chunk and
/// never more than `compound_limit` (which wins over the chunk floor, and can
/// be 0). Behind
/// [`Connection::quick_write_limit`](crate::client::Connection::quick_write_limit),
/// which documents the reasoning.
pub(crate) fn quick_write_limit(rate: Option<f64>, compound_limit: u64) -> u64 {
    quick_limit(rate, UPLOAD_CHUNK_SIZE, compound_limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHUNK: u64 = UPLOAD_CHUNK_SIZE as u64;
    const EIGHT_MIB: u64 = 8 << 20;

    #[test]
    fn with_no_rate_one_chunk_is_quick() {
        assert_eq!(quick_write_limit(None, EIGHT_MIB), CHUNK);
    }

    #[test]
    fn a_fast_uplink_makes_what_it_moves_in_the_headroom_quick() {
        // 16 MB/s moves 4 MB in 250 ms.
        assert_eq!(quick_write_limit(Some(16e6), EIGHT_MIB), 4_000_000);
    }

    #[test]
    fn a_slow_uplink_still_writes_one_chunk_in_one_go() {
        // 375 KB/s moves 94 KB in 250 ms, but one chunk is one WRITE either way.
        assert_eq!(quick_write_limit(Some(375e3), EIGHT_MIB), CHUNK);
    }

    #[test]
    fn one_frame_never_carries_more_than_the_compound_write_limit() {
        assert_eq!(quick_write_limit(Some(1e9), EIGHT_MIB), EIGHT_MIB);
        assert_eq!(quick_write_limit(None, 65536), 65536);
        // A window too small for any compound: stream everything, the floor
        // notwithstanding.
        assert_eq!(quick_write_limit(Some(16e6), 0), 0);
        assert_eq!(quick_write_limit(None, 0), 0);
    }

    #[test]
    fn a_fixed_window_is_at_least_one() {
        assert_eq!(Pacing::from(WriteBehind::Fixed(0)).fixed_window(), Some(1));
        assert_eq!(
            Pacing::from(WriteBehind::SEQUENTIAL).fixed_window(),
            Some(1)
        );
        assert_eq!(Pacing::from(WriteBehind::Adaptive).fixed_window(), None);
    }
}
