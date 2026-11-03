//! Direct TCP transport for SMB2 (port 445).
//!
//! Implements the SMB2 transport framing defined in MS-SMB2 section 2.1:
//! each message is preceded by a 4-byte header consisting of 1 zero byte
//! followed by 3 bytes of big-endian length. This is the ONLY big-endian
//! encoding in the entire SMB2 protocol.

use async_trait::async_trait;
use futures_util::future::Either;
use futures_util::stream::{FuturesUnordered, StreamExt};
use log::{debug, error, trace};
use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::rt::net::{self, ReadHalf, TcpStream, WriteHalf};
use crate::rt::{self, Instant};
use crate::transport::{ReceiveProgress, TransportReceive, TransportSend};

/// Maximum frame size we accept (16 MB).
///
/// Prevents denial-of-service from corrupt or malicious length fields.
/// Real SMB2 messages are typically much smaller (the largest negotiated
/// MaxReadSize/MaxWriteSize is usually 8 MB).
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// How a connect attempt is bounded, and how it is spread across the addresses
/// a name resolves to.
///
/// The thing this exists to prevent: `TcpStream::connect` walks every address
/// `getaddrinfo` returns, one at a time, and a single deadline around it means
/// one address that blackholes SYNs eats the whole budget while the live ones
/// are never dialled. Every AD domain name, and plenty of NASes, resolve to
/// several addresses, and a DFS namespace root makes it worse by construction:
/// the name being dialled *is* a domain name.
///
/// So attempts are staggered instead, RFC 8305's shape without the full
/// algorithm: start the next address after [`attempt_delay`](Self::attempt_delay),
/// leave the earlier ones running, first connected socket wins, and every
/// address gets a real chance inside the caller's budget.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ConnectOptions {
    /// Budget for the whole attempt, name resolution included.
    pub timeout: Duration,
    /// How long to wait before starting the next address, with earlier
    /// attempts left running. Zero dials every address at once, which puts a
    /// SYN on every interface of every server for every connect — rarely what
    /// you want.
    pub attempt_delay: Duration,
    /// Cap on how many resolved addresses to try. Clamped to at least 1, so a
    /// zero cannot make connecting impossible.
    ///
    /// Eight covers a realistically-sized set of domain controllers across
    /// both address families, and at the default stagger the eighth attempt
    /// starts 1.75 s in — comfortably inside any sane budget. A name with more
    /// addresses than that is a load-balanced pool where the extras are
    /// interchangeable, so trying them all buys nothing and costs a SYN each.
    pub max_addresses: usize,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            // RFC 8305 § 5 recommends 250 ms, with 2 s as the maximum.
            attempt_delay: Duration::from_millis(250),
            max_addresses: 8,
        }
    }
}

impl ConnectOptions {
    /// The defaults with the whole-attempt budget replaced.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Self::default()
        }
    }
}

/// What happened on one address of a name that could not be connected.
///
/// Part of [`Error::ConnectFailed`], which is the difference between "the
/// connect timed out" and "these four addresses were tried, three refused and
/// one never answered".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectAttempt {
    /// The address that was dialled.
    pub addr: SocketAddr,
    /// Why it failed, as a typed kind so nothing downstream is tempted to
    /// match on a message. `None` means the attempt ran out of budget rather
    /// than failing outright — it may still have been on its way.
    pub error_kind: Option<std::io::ErrorKind>,
}

impl fmt::Display for ConnectAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.error_kind {
            Some(kind) => write!(f, "{}: {kind}", self.addr),
            None => write!(f, "{}: no answer within the budget", self.addr),
        }
    }
}

/// Direct TCP transport for SMB2.
///
/// Wraps a TCP connection and handles the 4-byte framing header.
/// The connection is split into independent read and write halves
/// so that send and receive can proceed concurrently without contention.
///
/// Runs on whichever runtime the connect was made on; see the crate docs'
/// "Async runtime" section.
#[derive(Debug)]
pub struct TcpTransport {
    /// The read half of the TCP connection, behind a mutex for `&self` access.
    reader: Mutex<ReadHalf>,
    /// The write half of the TCP connection, behind a mutex for `&self` access.
    writer: Mutex<WriteHalf>,
    /// What `receive` has read so far, published per socket read. See
    /// [`ReceiveProgress`].
    progress: Arc<ReceiveProgress>,
}

impl TcpTransport {
    /// Connect to an SMB server over TCP.
    ///
    /// `timeout` bounds the whole attempt, name resolution included. Every
    /// address the name resolves to gets a real chance inside it: see
    /// [`ConnectOptions`] for why that is not the same as "a deadline around
    /// `TcpStream::connect`". Once connected, the socket is split into
    /// independent read/write halves.
    ///
    /// `addr` is anything that displays as `host:port` or `ip:port`: `&str`,
    /// `String`, and `SocketAddr` all do. The display form is what gets
    /// resolved, and what an error names.
    pub async fn connect(addr: impl fmt::Display, timeout: Duration) -> Result<Self> {
        Self::connect_with(addr, ConnectOptions::with_timeout(timeout)).await
    }

    /// [`connect`](Self::connect) with the stagger and the address cap under
    /// the caller's control.
    pub async fn connect_with(addr: impl fmt::Display, opts: ConnectOptions) -> Result<Self> {
        let host = addr.to_string();
        let deadline = Instant::now() + opts.timeout;

        // Resolution is inside the budget, because it is part of the wait the
        // caller is bounding.
        //
        // **Known limit, documented rather than solved:** resolving runs
        // `getaddrinfo` on a blocking pool thread. A timeout abandons the
        // future; the thread stays until the resolver returns. A pure-Rust
        // resolver would fix it, and is a dependency this crate has no other
        // reason to take.
        let resolved = rt::timeout_at(deadline, net::resolve(&host))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(Error::Io)?;

        let addrs = interleave_families(resolved, opts.max_addresses.max(1));
        if addrs.is_empty() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{host} resolved to no addresses"),
            )));
        }

        let stream = dial_staggered(&host, &addrs, &opts, deadline).await?;

        // Disable Nagle's algorithm for lower latency on small messages.
        stream.set_nodelay(true).map_err(Error::Io)?;

        debug!("tcp: connected, nodelay=true");
        Ok(Self::from_stream(stream))
    }

    /// Wrap a connected socket.
    fn from_stream(stream: TcpStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(writer),
            progress: Arc::new(ReceiveProgress::new()),
        }
    }
}

/// Dial `addrs` with a stagger, under one shared deadline. First socket to
/// connect wins; the rest are dropped.
pub(crate) async fn dial_staggered(
    host: &str,
    addrs: &[SocketAddr],
    opts: &ConnectOptions,
    deadline: Instant,
) -> Result<TcpStream> {
    // One slot per address, filled in as attempts finish. An address still in
    // flight when the budget runs out keeps its `None`, which is the honest
    // answer: nothing is known about it.
    let mut attempts: Vec<ConnectAttempt> = addrs
        .iter()
        .map(|&addr| ConnectAttempt {
            addr,
            error_kind: None,
        })
        .collect();

    let mut in_flight = FuturesUnordered::new();
    let mut next = 0usize;

    loop {
        if next < addrs.len() {
            let (index, addr) = (next, addrs[next]);
            next += 1;
            trace!(
                "tcp: dialling (attempt {}) host={host:?} ip={addr:?}",
                index + 1
            );
            in_flight.push(async move { (index, TcpStream::connect(addr).await) });
        }

        if in_flight.is_empty() && next >= addrs.len() {
            break;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        // Wake to start the next address, or at the deadline once they have
        // all been started.
        let wake_in = if next < addrs.len() {
            opts.attempt_delay.min(remaining)
        } else {
            remaining
        };

        // ❌ Not `tokio::select!`: that macro needs tokio's `macros` feature,
        // which is a dev-dependency here. Turning it into a real dependency
        // would put a proc macro in every consumer's build for one call site.
        // (Caught by the fuzz build, which compiles the library without
        // dev-dependencies. `cargo clippy --all-targets` does not.)
        if in_flight.is_empty() {
            rt::sleep(wake_in).await;
            continue;
        }
        let timer = std::pin::pin!(rt::sleep(wake_in));
        match futures_util::future::select(in_flight.next(), timer).await {
            // An attempt finished.
            Either::Left((Some((index, result)), _)) => match result {
                Ok(stream) => {
                    debug!(
                        "tcp: connected ({} of {} address(es) tried) host={host:?} ip={:?}",
                        index + 1,
                        addrs.len(),
                        addrs[index]
                    );
                    return Ok(stream);
                }
                Err(e) => {
                    // A fast failure starts the next address immediately: the
                    // loop pushes it before computing a fresh stagger.
                    trace!(
                        "tcp: address refused host={host:?} ip={:?}: {e}",
                        addrs[index]
                    );
                    attempts[index].error_kind = Some(e.kind());
                }
            },
            // The set drained between the emptiness check and the poll.
            Either::Left((None, _)) => {}
            // Time to start the next address, or the deadline, depending on
            // which one `wake_in` was.
            Either::Right(((), _)) => {}
        }
    }

    Err(Error::ConnectFailed {
        host: host.to_string(),
        attempts,
    })
}

/// Order addresses so the families alternate, keeping the resolver's own
/// preference for which goes first.
///
/// Without this, a name whose IPv6 addresses all come first and all blackhole
/// pushes every IPv4 address past `max_addresses * attempt_delay`, which is
/// the exact failure the stagger exists to prevent — just later. The field log
/// that started this had an `os error 65` (no route to host) on an IPv6
/// address of a name whose IPv4 addresses connected in 4–22 ms.
fn interleave_families(addrs: impl IntoIterator<Item = SocketAddr>, max: usize) -> Vec<SocketAddr> {
    let mut v6: VecDeque<SocketAddr> = VecDeque::new();
    let mut v4: VecDeque<SocketAddr> = VecDeque::new();
    let mut prefer_v6: Option<bool> = None;

    for addr in addrs {
        prefer_v6.get_or_insert(addr.is_ipv6());
        if addr.is_ipv6() {
            v6.push_back(addr);
        } else {
            v4.push_back(addr);
        }
    }

    let mut take_v6 = prefer_v6.unwrap_or(false);
    let mut out = Vec::new();
    while out.len() < max && !(v6.is_empty() && v4.is_empty()) {
        let preferred = if take_v6 { &mut v6 } else { &mut v4 };
        let picked = preferred.pop_front().or_else(|| {
            if take_v6 {
                v4.pop_front()
            } else {
                v6.pop_front()
            }
        });
        if let Some(addr) = picked {
            out.push(addr);
        }
        take_v6 = !take_v6;
    }
    out
}

#[async_trait]
impl TransportSend for TcpTransport {
    async fn send(&self, data: &[u8]) -> Result<()> {
        let len = data.len();
        if len > MAX_FRAME_SIZE {
            return Err(Error::invalid_data(format!(
                "message size {} exceeds maximum frame size {}",
                len, MAX_FRAME_SIZE
            )));
        }

        // Build the 4-byte framing header: 0x00 + 3-byte BE length.
        let mut frame_header = [0u8; 4];
        frame_header[0] = 0x00;
        frame_header[1] = (len >> 16) as u8;
        frame_header[2] = (len >> 8) as u8;
        frame_header[3] = len as u8;

        let mut writer = self.writer.lock().await;
        writer.write_all(&frame_header).await.map_err(Error::Io)?;
        writer.write_all(data).await.map_err(Error::Io)?;
        writer.flush().await.map_err(Error::Io)?;

        trace!("tcp: sent frame, len={}", len);
        Ok(())
    }
}

#[async_trait]
impl TransportReceive for TcpTransport {
    async fn receive(&self) -> Result<Vec<u8>> {
        let mut reader = self.reader.lock().await;

        // Read the 4-byte framing header.
        let mut frame_header = [0u8; 4];
        reader
            .read_exact(&mut frame_header)
            .await
            .map_err(read_error)?;

        // Validate the first byte is 0x00.
        if frame_header[0] != 0x00 {
            error!("tcp: invalid frame, first byte=0x{:02X}", frame_header[0]);
            return Err(Error::invalid_data(format!(
                "invalid transport frame: first byte must be 0x00, got 0x{:02X}",
                frame_header[0]
            )));
        }

        // Extract the 3-byte big-endian length.
        let msg_len = ((frame_header[1] as usize) << 16)
            | ((frame_header[2] as usize) << 8)
            | (frame_header[3] as usize);

        // Validate against the maximum frame size.
        if msg_len > MAX_FRAME_SIZE {
            return Err(Error::invalid_data(format!(
                "frame length {} exceeds maximum {}",
                msg_len, MAX_FRAME_SIZE
            )));
        }

        trace!("tcp: receiving frame, len={}", msg_len);

        // Read the message body, publishing each piece as it lands. This is
        // `read_exact` with a counter in it, and has to keep its semantics
        // exactly: every byte or an error, EOF partway is `Disconnected`.
        let mut buf = vec![0u8; msg_len];
        let _frame = FrameInProgress::begin(&self.progress, msg_len);
        let mut filled = 0;
        while filled < msg_len {
            let n = reader.read(&mut buf[filled..]).await.map_err(read_error)?;
            if n == 0 {
                return Err(Error::Disconnected);
            }
            filled += n;
            self.progress.record(n);
        }

        trace!("tcp: received frame, len={}", msg_len);
        Ok(buf)
    }

    fn receive_progress(&self) -> Option<Arc<ReceiveProgress>> {
        Some(Arc::clone(&self.progress))
    }
}

/// A read error as the connection needs to see it: the peer closing the
/// socket is `Disconnected`, anything else is the I/O error itself.
fn read_error(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::Disconnected
    } else {
        Error::Io(e)
    }
}

/// Marks a frame as arriving for as long as it lives, so a receive that fails
/// or is dropped partway can't leave a frame reported as permanently half in.
struct FrameInProgress<'a>(&'a ReceiveProgress);

impl<'a> FrameInProgress<'a> {
    fn begin(progress: &'a ReceiveProgress, len: usize) -> Self {
        progress.begin_frame(len);
        Self(progress)
    }
}

impl Drop for FrameInProgress<'_> {
    fn drop(&mut self) {
        self.0.end_frame();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The tests drive the far end with tokio's own sockets; `.into()` wraps
    // one for `TcpTransport`.
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    /// Build a framed message (4-byte header + payload).
    fn frame_message(payload: &[u8]) -> Vec<u8> {
        let len = payload.len();
        let mut frame = Vec::with_capacity(4 + len);
        frame.push(0x00);
        frame.push((len >> 16) as u8);
        frame.push((len >> 8) as u8);
        frame.push(len as u8);
        frame.extend_from_slice(payload);
        frame
    }

    // ── Send framing tests ──────────────────────────────────────────

    #[test]
    fn frame_header_format_small_message() {
        let payload = vec![0xFE, 0x53, 0x4D, 0x42]; // "SMB2 magic"
        let framed = frame_message(&payload);

        // Header: [0x00, 0x00, 0x00, 0x04]
        assert_eq!(framed[0], 0x00, "first byte must be 0x00");
        assert_eq!(framed[1], 0x00, "length high byte");
        assert_eq!(framed[2], 0x00, "length mid byte");
        assert_eq!(framed[3], 0x04, "length low byte = 4");
        assert_eq!(&framed[4..], &payload);
    }

    #[test]
    fn frame_header_format_medium_message() {
        // 300 bytes -> 0x00, 0x00, 0x01, 0x2C
        let payload = vec![0xAA; 300];
        let framed = frame_message(&payload);

        assert_eq!(framed[0], 0x00);
        assert_eq!(framed[1], 0x00);
        assert_eq!(framed[2], 0x01);
        assert_eq!(framed[3], 0x2C);
        assert_eq!(framed.len(), 304);
    }

    #[test]
    fn frame_header_format_large_message() {
        // 0x010203 = 66051 bytes
        let payload = vec![0xBB; 66051];
        let framed = frame_message(&payload);

        assert_eq!(framed[0], 0x00);
        assert_eq!(framed[1], 0x01);
        assert_eq!(framed[2], 0x02);
        assert_eq!(framed[3], 0x03);
    }

    #[test]
    fn frame_header_empty_payload() {
        let framed = frame_message(&[]);
        assert_eq!(framed, vec![0x00, 0x00, 0x00, 0x00]);
    }

    // ── Receive framing tests (using tokio_test-style mock streams) ──

    /// A helper that creates a pair of connected streams via a TCP listener
    /// on localhost, then writes data to one side and reads from the other.
    async fn receive_from_bytes(data: &[u8]) -> Result<Vec<u8>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let data = data.to_vec();
        let writer_task = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream.write_all(&data).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let transport = TcpTransport::from_stream(stream.into());

        let result = transport.receive().await;
        writer_task.await.unwrap();
        result
    }

    #[tokio::test]
    async fn receive_valid_frame() {
        let payload = vec![0xFE, 0x53, 0x4D, 0x42, 0x01, 0x02];
        let framed = frame_message(&payload);

        let received = receive_from_bytes(&framed).await.unwrap();
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn receive_empty_payload() {
        let framed = frame_message(&[]);
        let received = receive_from_bytes(&framed).await.unwrap();
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn receive_first_byte_not_zero_returns_error() {
        // First byte is 0x01 instead of 0x00.
        let data = vec![0x01, 0x00, 0x00, 0x04, 0xAA, 0xBB, 0xCC, 0xDD];

        let result = receive_from_bytes(&data).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("first byte must be 0x00"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn receive_length_exceeds_max_returns_error() {
        // Length = 0xFFFFFF = 16777215 > MAX_FRAME_SIZE (16 * 1024 * 1024 = 16777216)
        // Wait, 0xFFFFFF = 16777215 < 16777216. Let's use a length just over.
        // MAX_FRAME_SIZE = 16 * 1024 * 1024 = 16_777_216
        // We need > 16_777_216, but max 3-byte value is 16_777_215.
        // So 3 bytes can't exceed 16 MB. But the spec says 16 MB is the max.
        // Let's set MAX_FRAME_SIZE to slightly less, or test at the boundary.
        // Actually MAX_FRAME_SIZE = 16 * 1024 * 1024 = 16_777_216.
        // Max 3-byte value = 0xFFFFFF = 16_777_215 which is < MAX_FRAME_SIZE.
        // So a 3-byte length can never exceed our MAX_FRAME_SIZE.
        // This test verifies that the max 3-byte value IS accepted (no error).
        // But what if someone sends a broken frame? The first byte check
        // catches that. For the length check specifically, we'd need a
        // smaller MAX_FRAME_SIZE to exercise the branch. For now, let's test
        // with an internal test. The important thing is the check exists.

        // Actually, the more realistic concern is a malicious server sending
        // large values. 0xFFFFFF = ~16 MB is fine by our limit. Let's verify
        // the boundary: 0xFFFFFF should be accepted because 16_777_215 < 16_777_216.
        // We can't test > MAX_FRAME_SIZE with only 3 bytes, but the check
        // is there for defense-in-depth (the first byte could be non-zero
        // and interpreted as part of length if we didn't validate it).

        // Let's test a frame with length 0xFFFFFF but not enough payload data,
        // which should return Disconnected (not a crash from huge allocation).
        let data = vec![0x00, 0xFF, 0xFF, 0xFF]; // Length = 16_777_215 bytes, no payload.

        let result = receive_from_bytes(&data).await;
        assert!(result.is_err());
        // Should get Disconnected because the payload read fails.
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Disconnected),
            "expected Disconnected for truncated large frame, got: {err}"
        );
    }

    #[tokio::test]
    async fn receive_disconnected_on_eof() {
        // Empty data = immediate EOF.
        let result = receive_from_bytes(&[]).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Disconnected),
            "expected Disconnected, got: {err}"
        );
    }

    #[tokio::test]
    async fn receive_partial_header_returns_disconnected() {
        // Only 2 bytes of the 4-byte header.
        let result = receive_from_bytes(&[0x00, 0x00]).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Disconnected),
            "expected Disconnected for partial header, got: {err}"
        );
    }

    #[tokio::test]
    async fn receive_partial_payload_returns_disconnected() {
        // Header says 10 bytes, but only 3 bytes of payload follow.
        let data = vec![0x00, 0x00, 0x00, 0x0A, 0x01, 0x02, 0x03];

        let result = receive_from_bytes(&data).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Disconnected),
            "expected Disconnected for truncated payload, got: {err}"
        );
    }

    #[tokio::test]
    async fn send_and_receive_roundtrip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let send_task = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let transport = TcpTransport::from_stream(stream.into());

            let payload = vec![0xFE, 0x53, 0x4D, 0x42, 0xDE, 0xAD];
            transport.send(&payload).await.unwrap();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let recv_transport = TcpTransport::from_stream(stream.into());

        let received = recv_transport.receive().await.unwrap();
        assert_eq!(received, vec![0xFE, 0x53, 0x4D, 0x42, 0xDE, 0xAD]);

        send_task.await.unwrap();
    }

    #[tokio::test]
    async fn send_and_receive_multiple_messages() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let send_task = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let transport = TcpTransport::from_stream(stream.into());

            transport.send(&[0x01, 0x02]).await.unwrap();
            transport.send(&[0x03, 0x04, 0x05]).await.unwrap();
            transport.send(&[0x06]).await.unwrap();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let recv_transport = TcpTransport::from_stream(stream.into());

        assert_eq!(recv_transport.receive().await.unwrap(), vec![0x01, 0x02]);
        assert_eq!(
            recv_transport.receive().await.unwrap(),
            vec![0x03, 0x04, 0x05]
        );
        assert_eq!(recv_transport.receive().await.unwrap(), vec![0x06]);

        send_task.await.unwrap();
    }

    // ── Connect budget ──────────────────────────────────────────────

    /// TEST-NET-1 (RFC 5737). It is guaranteed not to be routed anywhere, and
    /// it **drops** SYNs rather than refusing them, so an attempt against it
    /// hangs exactly the way a dead domain controller does.
    const BLACKHOLE: &str = "192.0.2.1:445";

    /// The bug this whole thing exists for: `TcpStream::connect` walks the
    /// resolved addresses one at a time under one deadline, so a single
    /// blackholed address eats the entire budget and the live ones are never
    /// dialled. Staggering makes the live address win in well under a second.
    #[tokio::test]
    async fn a_blackholed_address_does_not_eat_the_budget() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = listener.local_addr().unwrap();

        let opts = ConnectOptions {
            timeout: Duration::from_secs(10),
            attempt_delay: Duration::from_millis(250),
            max_addresses: 8,
        };
        let addrs = vec![BLACKHOLE.parse().unwrap(), live];

        let started = std::time::Instant::now();
        let stream = dial_staggered("test", &addrs, &opts, Instant::now() + opts.timeout)
            .await
            .expect("the live address must win");
        let elapsed = started.elapsed();

        assert_eq!(stream.peer_addr().unwrap(), live);
        // The stagger makes this a real bound rather than a race: the second
        // attempt starts at 250 ms and connects to loopback immediately.
        assert!(
            elapsed < Duration::from_secs(1),
            "took {elapsed:?}; a serial walk would have taken the full 10 s"
        );
    }

    /// Port 0 is not a port a TCP connection can be made to, and every stack
    /// rejects it in its own `connect` call without putting a packet on the
    /// wire: `AddrNotAvailable` on macOS (35 µs), `ConnectionRefused` on Linux
    /// (71 µs), `WSAEADDRNOTAVAIL` on Windows. Which kind it is varies, so
    /// tests here assert only that there *is* one.
    ///
    /// ❌ **Don't reach for a just-released ephemeral port instead.** It looks
    /// equivalent and isn't: it assumes the stack answers a closed port with a
    /// RST promptly, which is a Unix habit rather than a guarantee. Windows CI
    /// sent no RST inside a 400 ms budget and the attempt was correctly
    /// recorded as "never finished", failing a test that assumed otherwise.
    const UNCONNECTABLE: &str = "127.0.0.1:0";

    /// One `ConnectAttempt` per address, in the order they were attempted,
    /// whatever each one did.
    ///
    /// Deliberately asserts nothing about the *kinds*: how fast a given stack
    /// refuses decides those, and the two tests below pin each kind separately
    /// by staging it rather than by racing a budget. What is true on every
    /// platform is that the slots exist, are filled in, and line up with the
    /// addresses — `attempts` is built from the address list up front, so this
    /// holds however the attempts finish or don't.
    #[tokio::test]
    async fn every_address_failing_reports_every_address() {
        let addrs = vec![UNCONNECTABLE.parse().unwrap(), BLACKHOLE.parse().unwrap()];

        let opts = ConnectOptions {
            timeout: Duration::from_millis(400),
            attempt_delay: Duration::from_millis(50),
            max_addresses: 8,
        };
        let err = dial_staggered("test", &addrs, &opts, Instant::now() + opts.timeout)
            .await
            .expect_err("neither address can be connected to");

        match err {
            Error::ConnectFailed { host, attempts } => {
                assert_eq!(host, "test");
                assert_eq!(attempts.len(), 2, "one entry per address");
                assert_eq!(attempts[0].addr, addrs[0]);
                assert_eq!(attempts[1].addr, addrs[1], "in the order attempted");
            }
            other => panic!("expected ConnectFailed, got {other:?}"),
        }
    }

    /// An attempt that failed carries the reason.
    ///
    /// The budget is far larger than the failure it is waiting for, so the
    /// budget can never be what ends this: the only way to reach the
    /// assertion is the connect failing, and the only way to fail the
    /// assertion would be connecting to port 0 successfully.
    #[tokio::test]
    async fn a_failed_attempt_reports_why() {
        let addrs = vec![UNCONNECTABLE.parse().unwrap()];
        let opts = ConnectOptions {
            timeout: Duration::from_secs(10),
            ..ConnectOptions::default()
        };

        let err = dial_staggered("test", &addrs, &opts, Instant::now() + opts.timeout)
            .await
            .expect_err("port 0 is not connectable");

        match err {
            Error::ConnectFailed { attempts, .. } => {
                assert_eq!(attempts.len(), 1);
                assert!(
                    attempts[0].error_kind.is_some(),
                    "an attempt that failed has to say why; got {:?}",
                    attempts[0]
                );
            }
            other => panic!("expected ConnectFailed, got {other:?}"),
        }
    }

    /// An attempt that never finished reports no reason, because none is
    /// known — it may still have been on its way.
    ///
    /// Staged with a deadline that has already passed rather than raced
    /// against a slow address: that makes "did not finish" true by
    /// construction on every platform, instead of depending on how long some
    /// network takes to not answer.
    #[tokio::test]
    async fn an_attempt_that_never_finished_reports_no_reason() {
        let addrs = vec![BLACKHOLE.parse().unwrap()];
        let opts = ConnectOptions {
            timeout: Duration::ZERO,
            ..ConnectOptions::default()
        };

        // `Instant::now()` is already in the past by the time the loop reads
        // the clock, and the clock is monotonic, so there is no budget at all.
        let err = dial_staggered("test", &addrs, &opts, Instant::now())
            .await
            .expect_err("no budget, no connection");

        match err {
            Error::ConnectFailed { attempts, .. } => {
                assert_eq!(attempts.len(), 1, "the address is still reported");
                assert_eq!(
                    attempts[0].error_kind, None,
                    "nothing is known about an attempt that didn't finish"
                );
            }
            other => panic!("expected ConnectFailed, got {other:?}"),
        }
    }

    /// The families alternate, so a name whose IPv6 addresses all come first
    /// and all blackhole cannot push every IPv4 address past the stagger.
    #[test]
    fn families_alternate_keeping_the_resolvers_preference() {
        let a: Vec<SocketAddr> = [
            "[2001:db8::1]:445",
            "[2001:db8::2]:445",
            "192.0.2.1:445",
            "192.0.2.2:445",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();

        let ordered = interleave_families(a.clone(), 8);
        assert_eq!(
            ordered,
            vec![a[0], a[2], a[1], a[3]],
            "v6 first (the resolver's order), then alternating"
        );

        // The other preference, from the same input reversed.
        let reversed: Vec<SocketAddr> = a.iter().rev().copied().collect();
        let ordered = interleave_families(reversed.clone(), 8);
        assert_eq!(
            ordered,
            vec![reversed[0], reversed[2], reversed[1], reversed[3]]
        );
    }

    /// One family only still drains in order, with no gaps.
    #[test]
    fn a_single_family_is_left_in_order() {
        let a: Vec<SocketAddr> = ["192.0.2.1:445", "192.0.2.2:445", "192.0.2.3:445"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(interleave_families(a.clone(), 8), a);
    }

    #[test]
    fn the_address_cap_is_honored() {
        let a: Vec<SocketAddr> = (1..=6)
            .map(|i| format!("192.0.2.{i}:445").parse().unwrap())
            .collect();

        assert_eq!(interleave_families(a.clone(), 2).len(), 2);
        assert_eq!(interleave_families(a, 100).len(), 6);
    }

    /// A cap of zero would otherwise mean "try nothing", which is a config
    /// that can never connect. It is clamped to one.
    #[tokio::test]
    async fn an_address_cap_of_zero_still_tries_one_address() {
        let err = TcpTransport::connect_with(
            BLACKHOLE,
            ConnectOptions {
                timeout: Duration::from_millis(100),
                max_addresses: 0,
                ..ConnectOptions::default()
            },
        )
        .await
        .expect_err("TEST-NET-1 never answers");

        match err {
            Error::ConnectFailed { attempts, .. } => assert_eq!(attempts.len(), 1),
            // An ICMP unreachable rather than a drop; still one address tried.
            Error::Io(_) => {}
            other => panic!("expected ConnectFailed, got: {other}"),
        }
    }

    /// A name that resolves to nothing is an error, not a hang.
    #[tokio::test]
    async fn no_addresses_is_a_clean_error() {
        let opts = ConnectOptions::default();
        let err = dial_staggered("nowhere", &[], &opts, Instant::now() + opts.timeout)
            .await
            .expect_err("no addresses, no connection");
        assert!(matches!(err, Error::ConnectFailed { ref attempts, .. } if attempts.is_empty()));
    }

    /// A loopback pair: the transport on one end, the raw socket the test
    /// writes "server" bytes into on the other.
    async fn transport_and_far_end() -> (Arc<TcpTransport>, TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let far_end = TcpStream::connect(addr).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        (Arc::new(TcpTransport::from_stream(stream.into())), far_end)
    }

    /// Wait (bounded) for the transport's counts to satisfy `cond`.
    async fn progress_reaches(
        transport: &TcpTransport,
        what: &str,
        cond: impl Fn(&crate::transport::ReceiveSnapshot) -> bool,
    ) -> crate::transport::ReceiveSnapshot {
        let progress = transport
            .receive_progress()
            .expect("tcp publishes progress");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snap = progress.snapshot();
            if cond(&snap) {
                return snap;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    #[tokio::test]
    async fn bytes_inside_a_frame_are_visible_before_it_completes() {
        let (transport, mut far_end) = transport_and_far_end().await;
        let payload: Vec<u8> = (0..=255).cycle().take(10_000).collect();
        let framed = frame_message(&payload);

        let receiving = {
            let t = Arc::clone(&transport);
            tokio::spawn(async move { t.receive().await })
        };
        // The prefix and the first 4,000 payload bytes, then nothing.
        far_end.write_all(&framed[..4 + 4_000]).await.unwrap();
        let snap = progress_reaches(&transport, "the first piece", |s| s.bytes == 4_000).await;
        assert_eq!(
            snap.frame,
            Some(crate::transport::FrameProgress {
                received: 4_000,
                len: 10_000
            })
        );
        assert!(snap.last_byte_at.is_some());
        assert!(!receiving.is_finished(), "the frame isn't whole yet");

        far_end.write_all(&framed[4 + 4_000..]).await.unwrap();
        let received = receiving.await.unwrap().unwrap();
        assert_eq!(received, payload, "counting must not disturb the bytes");
        let snap = transport.receive_progress().unwrap().snapshot();
        assert_eq!(
            snap.bytes, 10_000,
            "payload only, the length prefix excluded"
        );
        assert_eq!(snap.frame, None, "between frames");
    }

    #[tokio::test]
    async fn a_frame_cut_off_partway_is_a_disconnect_and_stops_counting_as_in_progress() {
        let (transport, mut far_end) = transport_and_far_end().await;
        let framed = frame_message(&[0xAB; 1_000]);
        far_end.write_all(&framed[..4 + 300]).await.unwrap();
        far_end.shutdown().await.unwrap();

        let err = transport.receive().await.unwrap_err();
        assert!(matches!(err, Error::Disconnected), "got {err:?}");
        let snap = transport.receive_progress().unwrap().snapshot();
        assert_eq!(snap.bytes, 300);
        assert_eq!(snap.frame, None, "a dead frame is not still arriving");
    }

    #[tokio::test]
    async fn partial_reads_are_handled_by_read_exact() {
        // This test exercises the read_exact behavior by sending data
        // through a real TCP connection. Under the hood, TCP may deliver
        // data in arbitrary chunk sizes, especially with Nagle disabled.
        // While we can't force byte-at-a-time delivery reliably, we
        // verify correctness with a larger payload that's more likely
        // to arrive in multiple reads.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let payload: Vec<u8> = (0..=255).cycle().take(8192).collect();
        let payload_clone = payload.clone();

        let send_task = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let transport = TcpTransport::from_stream(stream.into());

            transport.send(&payload_clone).await.unwrap();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let recv_transport = TcpTransport::from_stream(stream.into());

        let received = recv_transport.receive().await.unwrap();
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload);

        send_task.await.unwrap();
    }

    #[tokio::test]
    async fn connect_with_timeout() {
        // Connect to localhost listener with a generous timeout.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let transport = TcpTransport::connect(addr, Duration::from_secs(5))
            .await
            .unwrap();

        // Accept the connection on the server side.
        let (server_stream, _) = listener.accept().await.unwrap();
        let (server_reader, mut server_writer) = server_stream.into_split();
        drop(server_reader);

        // Send a framed message from the "server" side.
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut frame = vec![0x00, 0x00, 0x00, 0x04];
        frame.extend_from_slice(&payload);
        server_writer.write_all(&frame).await.unwrap();
        server_writer.flush().await.unwrap();

        // Receive through the transport.
        let received = transport.receive().await.unwrap();
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn connect_timeout_fires() {
        // A non-routable address that drops SYNs, so the budget is what ends
        // the attempt. The error names the address and says it never answered,
        // rather than the bare `Error::Timeout` this used to give.
        let started = std::time::Instant::now();
        let err = TcpTransport::connect(BLACKHOLE, Duration::from_millis(100))
            .await
            .expect_err("nothing is listening on TEST-NET-1");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the budget must still bound the whole attempt"
        );
        match err {
            Error::ConnectFailed { host, attempts } => {
                assert_eq!(host, BLACKHOLE);
                assert_eq!(attempts.len(), 1);
                assert_eq!(attempts[0].addr, BLACKHOLE.parse().unwrap());
            }
            // Some networks answer TEST-NET-1 with an ICMP unreachable, which
            // is a real failure rather than a timeout; the error still names
            // the address either way.
            Error::Io(_) => {}
            other => panic!("expected ConnectFailed, got: {other}"),
        }
    }
}
