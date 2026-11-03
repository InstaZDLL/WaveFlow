//! Sockets on the current call's backend: TCP for SMB and Kerberos, UDP for
//! Kerberos. See the parent module for how the backend is picked.
//!
//! Only what the crate uses is here, with tokio's semantics wherever the two
//! runtimes differ, because the rest of the crate was written against tokio.

use std::io;
use std::net::SocketAddr;

#[cfg(feature = "smol")]
use smol::io::{AsyncReadExt as _, AsyncWriteExt as _};
#[cfg(feature = "tokio")]
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{backend, Backend};

/// Resolve `host` (`name:port` or `ip:port`) to its addresses, in the
/// resolver's order. An address literal comes back without a lookup.
///
/// Both backends run `getaddrinfo` on a blocking thread pool, so abandoning
/// the future leaves that thread until the resolver returns.
pub(crate) async fn resolve(host: &str) -> io::Result<Vec<SocketAddr>> {
    match backend() {
        #[cfg(feature = "tokio")]
        Backend::Tokio => Ok(tokio::net::lookup_host(host).await?.collect()),
        #[cfg(feature = "smol")]
        Backend::Smol => smol::net::resolve(host).await,
    }
}

/// A connected TCP socket.
#[derive(Debug)]
pub(crate) struct TcpStream(TcpInner);

#[derive(Debug)]
enum TcpInner {
    #[cfg(feature = "tokio")]
    Tokio(tokio::net::TcpStream),
    #[cfg(feature = "smol")]
    Smol(smol::net::TcpStream),
}

impl TcpStream {
    /// Dial one address.
    pub(crate) async fn connect(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self(match backend() {
            #[cfg(feature = "tokio")]
            Backend::Tokio => TcpInner::Tokio(tokio::net::TcpStream::connect(addr).await?),
            #[cfg(feature = "smol")]
            Backend::Smol => TcpInner::Smol(smol::net::TcpStream::connect(addr).await?),
        }))
    }

    /// Resolve `host` and dial its addresses one at a time, first success
    /// wins. That's what `TcpStream::connect("name:port")` does on either
    /// runtime, and what the KDC client wants. ❌ Not for SMB connects:
    /// `TcpTransport` staggers its attempts, and why is in its docs.
    pub(crate) async fn connect_host(host: &str) -> io::Result<Self> {
        let mut last_err = None;
        for addr in resolve(host).await? {
            match Self::connect(addr).await {
                Ok(stream) => return Ok(stream),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{host} did not resolve to any address"),
            )
        }))
    }

    /// Turn Nagle's algorithm off (`true`) or on.
    pub(crate) fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        match &self.0 {
            #[cfg(feature = "tokio")]
            TcpInner::Tokio(stream) => stream.set_nodelay(nodelay),
            #[cfg(feature = "smol")]
            TcpInner::Smol(stream) => stream.set_nodelay(nodelay),
        }
    }

    /// The address this socket is connected to.
    #[cfg(test)]
    pub(crate) fn peer_addr(&self) -> io::Result<SocketAddr> {
        match &self.0 {
            #[cfg(feature = "tokio")]
            TcpInner::Tokio(stream) => stream.peer_addr(),
            #[cfg(feature = "smol")]
            TcpInner::Smol(stream) => stream.peer_addr(),
        }
    }

    /// Split into halves that can be used from two tasks at once. The socket
    /// closes once both halves are gone; see [`WriteHalf`] for what dropping
    /// the write half alone does.
    pub(crate) fn into_split(self) -> (ReadHalf, WriteHalf) {
        match self.0 {
            #[cfg(feature = "tokio")]
            TcpInner::Tokio(stream) => {
                let (read, write) = stream.into_split();
                (ReadHalf::Tokio(read), WriteHalf(WriteInner::Tokio(write)))
            }
            // A smol socket is reference-counted, so a clone is a half.
            #[cfg(feature = "smol")]
            TcpInner::Smol(stream) => (
                ReadHalf::Smol(stream.clone()),
                WriteHalf(WriteInner::Smol(stream)),
            ),
        }
    }
}

/// Tests build loopback pairs with tokio's own sockets.
#[cfg(all(test, feature = "tokio"))]
impl From<tokio::net::TcpStream> for TcpStream {
    fn from(stream: tokio::net::TcpStream) -> Self {
        Self(TcpInner::Tokio(stream))
    }
}

/// The reading half of a [`TcpStream`].
#[derive(Debug)]
pub(crate) enum ReadHalf {
    #[cfg(feature = "tokio")]
    Tokio(tokio::net::tcp::OwnedReadHalf),
    #[cfg(feature = "smol")]
    Smol(smol::net::TcpStream),
}

impl ReadHalf {
    /// Read what's there, up to `buf.len()` bytes. `Ok(0)` is end of stream.
    pub(crate) async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(feature = "tokio")]
            Self::Tokio(half) => half.read(buf).await,
            #[cfg(feature = "smol")]
            Self::Smol(stream) => stream.read(buf).await,
        }
    }

    /// Fill `buf` exactly. End of stream first is `UnexpectedEof`.
    pub(crate) async fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        match self {
            #[cfg(feature = "tokio")]
            Self::Tokio(half) => half.read_exact(buf).await.map(|_| ()),
            #[cfg(feature = "smol")]
            Self::Smol(stream) => stream.read_exact(buf).await,
        }
    }
}

/// The writing half of a [`TcpStream`].
///
/// **Dropping it shuts the socket's write side down** (a FIN), on both
/// backends. That is tokio's `OwnedWriteHalf` behavior, and smol's halves
/// share one socket that only closes when the last one goes, so the smol arm
/// does it by hand. Either way the fd itself closes once the read half is
/// gone too; `client/CLAUDE.md` § Socket lifetime is what guarantees that.
#[derive(Debug)]
pub(crate) struct WriteHalf(WriteInner);

#[derive(Debug)]
enum WriteInner {
    #[cfg(feature = "tokio")]
    Tokio(tokio::net::tcp::OwnedWriteHalf),
    #[cfg(feature = "smol")]
    Smol(smol::net::TcpStream),
}

impl WriteHalf {
    /// Write all of `buf`.
    pub(crate) async fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match &mut self.0 {
            #[cfg(feature = "tokio")]
            WriteInner::Tokio(half) => half.write_all(buf).await,
            #[cfg(feature = "smol")]
            WriteInner::Smol(stream) => stream.write_all(buf).await,
        }
    }

    /// Push anything buffered to the socket.
    pub(crate) async fn flush(&mut self) -> io::Result<()> {
        match &mut self.0 {
            #[cfg(feature = "tokio")]
            WriteInner::Tokio(half) => half.flush().await,
            #[cfg(feature = "smol")]
            WriteInner::Smol(stream) => stream.flush().await,
        }
    }
}

impl Drop for WriteHalf {
    fn drop(&mut self) {
        match &self.0 {
            // `OwnedWriteHalf` shuts the write side down on drop by itself.
            #[cfg(feature = "tokio")]
            WriteInner::Tokio(_) => {}
            // Fails only on a socket that is already gone, which is the goal.
            #[cfg(feature = "smol")]
            WriteInner::Smol(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        }
    }
}

/// A UDP socket.
#[derive(Debug)]
pub(crate) struct UdpSocket(UdpInner);

#[derive(Debug)]
enum UdpInner {
    #[cfg(feature = "tokio")]
    Tokio(tokio::net::UdpSocket),
    #[cfg(feature = "smol")]
    Smol(smol::net::UdpSocket),
}

impl UdpSocket {
    /// Bind to `addr`.
    pub(crate) async fn bind(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self(match backend() {
            #[cfg(feature = "tokio")]
            Backend::Tokio => UdpInner::Tokio(tokio::net::UdpSocket::bind(addr).await?),
            #[cfg(feature = "smol")]
            Backend::Smol => UdpInner::Smol(smol::net::UdpSocket::bind(addr).await?),
        }))
    }

    /// Send `buf` to the first address `target` resolves to, which is what
    /// `send_to("name:port")` does on either runtime.
    pub(crate) async fn send_to(&self, buf: &[u8], target: &str) -> io::Result<usize> {
        let addr = resolve(target).await?.into_iter().next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{target} did not resolve to any address"),
            )
        })?;
        match &self.0 {
            #[cfg(feature = "tokio")]
            UdpInner::Tokio(socket) => socket.send_to(buf, addr).await,
            #[cfg(feature = "smol")]
            UdpInner::Smol(socket) => socket.send_to(buf, addr).await,
        }
    }

    /// Receive one datagram into `buf`.
    pub(crate) async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        match &self.0 {
            #[cfg(feature = "tokio")]
            UdpInner::Tokio(socket) => socket.recv_from(buf).await,
            #[cfg(feature = "smol")]
            UdpInner::Smol(socket) => socket.recv_from(buf).await,
        }
    }
}
