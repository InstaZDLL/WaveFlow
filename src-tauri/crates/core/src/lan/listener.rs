//! The accepting side.

use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio_rustls::rustls::{self, ServerConfig};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use super::address::PeerFilter;
use super::backoff::Backoff;
use super::cert::Identity;
use super::code::Code;
use super::exchange::HostExchange;
use super::handshake::{
    message_type, GuestHandshake, HandshakeRefusal, HostHandshake, EXCHANGE_START,
};
use super::{provider, websocket_config, EXPORTER_LABEL, HANDSHAKE_VERSION};

/// TLS over a TCP connection, as the listener holds it.
pub type ServerStream = tokio_rustls::server::TlsStream<TcpStream>;

/// Connections still in their handshake at once; more are closed as they
/// arrive. A LAN session has a handful of guests.
const MAX_HANDSHAKES: usize = 32;

/// Admitted connections waiting for the feature to take them.
const QUEUE: usize = 16;

/// What a listener is for and whom it lets in.
#[derive(Debug, Clone)]
pub struct Config {
    /// What the listener is for (`"wavesync"`): a code for another context
    /// does not open it.
    pub context: String,
    /// The user's _Allow private VPN addresses_ ([`PeerFilter`]).
    pub allow_private_vpn: bool,
    /// The port to listen on; 0 lets the system pick one.
    pub port: u16,
    /// What a peer may take to finish TLS, the WebSocket and the exchange:
    /// [`super::HANDSHAKE_TIMEOUT`] outside tests.
    pub handshake_timeout: std::time::Duration,
}

/// How a connection got in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// It proved it holds the code.
    Code,
    /// It did not start the exchange: its first message, for the feature to
    /// judge (a returning guest's token). Nothing about it is verified.
    Unverified { first: String },
}

/// A connection past the address filter, TLS and, for [`Admission::Code`],
/// the exchange.
pub struct Accepted {
    pub peer: SocketAddr,
    pub admission: Admission,
    pub socket: WebSocketStream<ServerStream>,
}

impl std::fmt::Debug for Accepted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Accepted")
            .field("peer", &self.peer)
            .field("admission", &self.admission)
            .finish_non_exhaustive()
    }
}

/// What the listener tells its owner.
#[derive(Debug)]
pub enum Incoming {
    /// Boxed: the WebSocket and its TLS state are large, and the other
    /// variant is a number.
    Accepted(Box<Accepted>),
    /// Somebody failed the exchange; `total` is the count since the start,
    /// for the host's _Someone is trying wrong codes_.
    WrongCode { total: u32 },
}

/// A running listener. Dropping it stops accepting; connections already
/// handed over are their owner's.
pub struct Listener {
    port: u16,
    shared: Arc<Shared>,
    _stop: watch::Sender<bool>,
}

struct Shared {
    context: String,
    filter: PeerFilter,
    acceptor: TlsAcceptor,
    /// The code, and how many times it has been set: an exchange begun under
    /// one setting opens nothing once another has been made, even a setting
    /// back to the same code after closing admission.
    code: Mutex<(u64, Option<Code>)>,
    backoff: Mutex<Backoff>,
    allow_private_vpn: bool,
    wrong: AtomicU32,
    started: Instant,
    handshakes: Arc<Semaphore>,
    handshake_timeout: std::time::Duration,
}

/// Why a listener could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("binding the listener: {0}")]
    Bind(#[from] io::Error),
    #[error("TLS configuration: {0}")]
    Tls(#[from] rustls::Error),
}

impl Listener {
    /// Listen on every IPv4 interface, and every IPv6 one where the system
    /// has IPv6, presenting `identity`. Nobody gets in by the code until
    /// [`Self::set_code`] gives one.
    pub async fn start(
        identity: &Identity,
        config: Config,
    ) -> Result<(Self, mpsc::Receiver<Incoming>), StartError> {
        let (chain, key) = identity.rustls_parts();
        let tls = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        let v4 = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], config.port))).await?;
        let port = v4.local_addr()?.port();
        // The same port on IPv6, IPv6 only, so the two sockets never compete
        // for IPv4 peers. A system without IPv6 is served on IPv4 alone.
        let v6 = bind_v6(port)
            .map_err(|e| tracing::debug!(error = %e, "lan: no IPv6 listener"))
            .ok();

        let shared = Arc::new(Shared {
            context: config.context,
            filter: PeerFilter::for_this_machine(config.allow_private_vpn),
            acceptor: TlsAcceptor::from(Arc::new(tls)),
            code: Mutex::new((0, None)),
            backoff: Mutex::new(Backoff::default()),
            allow_private_vpn: config.allow_private_vpn,
            wrong: AtomicU32::new(0),
            started: Instant::now(),
            handshakes: Arc::new(Semaphore::new(MAX_HANDSHAKES)),
            handshake_timeout: config.handshake_timeout,
        });
        let (stop, stopped) = watch::channel(false);
        let (tx, rx) = mpsc::channel(QUEUE);
        for socket in std::iter::once(v4).chain(v6) {
            tokio::spawn(accept_loop(
                socket,
                shared.clone(),
                tx.clone(),
                stopped.clone(),
            ));
        }
        Ok((
            Self {
                port,
                shared,
                _stop: stop,
            },
            rx,
        ))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The code that opens the listener now, or `None` to admit nobody by
    /// code. A new code replaces the old one at once.
    pub fn set_code(&self, code: Option<Code>) {
        let mut current = self
            .shared
            .code
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *current = (current.0 + 1, code);
    }

    /// Failed exchanges since the start.
    pub fn wrong_codes(&self) -> u32 {
        self.shared.wrong.load(Ordering::Relaxed)
    }
}

fn bind_v6(port: u16) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(true)?;
    socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
    socket.listen(64)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

async fn accept_loop(
    socket: TcpListener,
    shared: Arc<Shared>,
    tx: mpsc::Sender<Incoming>,
    mut stopped: watch::Receiver<bool>,
) {
    loop {
        let (tcp, peer) = tokio::select! {
            _ = stopped.changed() => return,
            accepted = socket.accept() => match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::debug!(error = %e, "lan: accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        // Before a byte of TLS: the address decides. Logged by kind only. A
        // peer in the shared block is judged against the machine's interfaces
        // as they are now: the VPN may have come up after the listener did.
        let filter = if super::address::kind(peer.ip()) == super::address::Kind::Shared {
            PeerFilter::for_this_machine(shared.allow_private_vpn)
        } else {
            shared.filter
        };
        if !filter.admits(peer.ip()) {
            tracing::debug!(kind = ?super::address::kind(peer.ip()), "lan: peer refused by address");
            continue;
        }
        let Ok(permit) = shared.handshakes.clone().try_acquire_owned() else {
            tracing::debug!("lan: too many handshakes at once");
            continue;
        };
        let shared = shared.clone();
        let tx = tx.clone();
        let mut stopped = stopped.clone();
        tokio::spawn(async move {
            let _permit = permit;
            tokio::select! {
                _ = stopped.changed() => {}
                _ = tokio::time::timeout(shared.handshake_timeout, admit(tcp, peer, &shared, &tx)) => {}
            }
        });
    }
}

/// The handshake of one connection. Any failure just closes it.
async fn admit(tcp: TcpStream, peer: SocketAddr, shared: &Shared, tx: &mpsc::Sender<Incoming>) {
    let Ok(tls) = shared.acceptor.accept(tcp).await else {
        return;
    };
    let Some(exporter) = export(tls.get_ref().1) else {
        return;
    };
    let Ok(mut socket) =
        tokio_tungstenite::accept_async_with_config(tls, Some(websocket_config())).await
    else {
        return;
    };
    let Some(first) = next_text(&mut socket).await else {
        return;
    };
    if message_type(&first).as_deref() != Some(EXCHANGE_START) {
        let _ = tx
            .send(Incoming::Accepted(Box::new(Accepted {
                peer,
                admission: Admission::Unverified { first },
                socket,
            })))
            .await;
        return;
    }

    let refuse = |reason, retry_after_ms| HostHandshake::ExchangeRefused {
        reason,
        retry_after_ms,
    };
    let Ok(GuestHandshake::ExchangeStart {
        version,
        context,
        guest,
    }) = serde_json::from_str(&first)
    else {
        send(&mut socket, &refuse(HandshakeRefusal::Malformed, None)).await;
        return;
    };
    if version != HANDSHAKE_VERSION {
        send(&mut socket, &refuse(HandshakeRefusal::Version, None)).await;
        return;
    }
    if context != shared.context {
        send(&mut socket, &refuse(HandshakeRefusal::Context, None)).await;
        return;
    }
    let (generation, code) = shared
        .code
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let Some(code) = code else {
        send(&mut socket, &refuse(HandshakeRefusal::Closed, None)).await;
        return;
    };
    // Checked and reserved under one lock, answered after it: the guard must
    // not live across the send.
    let begun = shared
        .backoff
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .begin(peer.ip(), shared.now_ms());
    if let Err(wait) = begun {
        send(
            &mut socket,
            &refuse(HandshakeRefusal::RetryLater, Some(wait)),
        )
        .await;
        return;
    }
    // From here every way out but success counts as a wrong code, the
    // handshake timing out included (the attempt is dropped unsettled then):
    // a guest that stops after the host's confirmation, sends nonsense or
    // never answers must not get a free attempt.
    let attempt = Attempt {
        shared,
        tx,
        ip: peer.ip(),
        settled: false,
    };
    let verified = async {
        let guest = hex::decode(guest).ok()?;
        let (pending, message, confirm) =
            HostExchange::respond(&code, &shared.context, &guest, &exporter).ok()?;
        let reply = HostHandshake::ExchangeReply {
            host: hex::encode(message),
            confirm: hex::encode(confirm),
        };
        if !send(&mut socket, &reply).await {
            return None;
        }
        let text = next_text(&mut socket).await?;
        let GuestHandshake::ExchangeConfirm { confirm } = serde_json::from_str(&text).ok()? else {
            return None;
        };
        pending.verify(&hex::decode(confirm).ok()?).ok()
    }
    .await
    .is_some();

    if !verified {
        attempt.fail();
        send(&mut socket, &refuse(HandshakeRefusal::WrongCode, None)).await;
        return;
    }
    // The guest proved the code: whatever happens next is not a wrong code,
    // even a handshake that runs out while waiting for a place below.
    attempt.succeed();
    // A place for the connection before the guest is told it is in, so a
    // guest never hears "done" from a host that then drops it.
    let Ok(place) = tx.reserve().await else {
        return;
    };
    // The code the guest proved is still the one the host shows, under the
    // same setting: one replaced or withdrawn while the guest answered, or
    // while it waited for that place, opens nothing.
    let still = shared.code.lock().unwrap_or_else(PoisonError::into_inner).0 == generation;
    if !still {
        send(&mut socket, &refuse(HandshakeRefusal::Expired, None)).await;
        return;
    }
    if send(&mut socket, &HostHandshake::ExchangeDone).await {
        place.send(Incoming::Accepted(Box::new(Accepted {
            peer,
            admission: Admission::Code,
            socket,
        })));
    }
}

/// An exchange under way for one sender, settled exactly once. Dropped
/// unsettled (the handshake timed out, the listener stopped) it counts as a
/// wrong code.
struct Attempt<'a> {
    shared: &'a Shared,
    tx: &'a mpsc::Sender<Incoming>,
    ip: IpAddr,
    settled: bool,
}

impl Attempt<'_> {
    fn fail(mut self) {
        self.settled = true;
        self.shared.record_failure(self.ip, self.tx);
    }

    fn succeed(mut self) {
        self.settled = true;
        self.shared
            .backoff
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .succeeded(self.ip);
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.shared.record_failure(self.ip, self.tx);
        }
    }
}

impl Shared {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Count a wrong code from `ip` and tell the owner. The notice is not
    /// waited for: it may be dropped when the owner is behind, the count is
    /// not ([`Listener::wrong_codes`]).
    fn record_failure(&self, ip: IpAddr, tx: &mpsc::Sender<Incoming>) {
        let total = self
            .backoff
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed(ip, self.now_ms());
        // Two failures settled at once may publish out of order: keep the
        // highest, so the count never goes back.
        self.wrong.fetch_max(total, Ordering::Relaxed);
        let _ = tx.try_send(Incoming::WrongCode { total });
    }
}

/// The TLS session's exporter bytes for the exchange.
pub(super) fn export<D>(conn: &rustls::ConnectionCommon<D>) -> Option<Vec<u8>> {
    conn.export_keying_material(vec![0u8; 32], EXPORTER_LABEL, None)
        .ok()
}

/// The next text frame, skipping control frames; `None` on anything else.
pub(super) async fn next_text<S>(socket: &mut WebSocketStream<S>) -> Option<String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match socket.next().await? {
            Ok(Message::Text(text)) => return Some(text.as_str().to_owned()),
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            _ => return None,
        }
    }
}

/// Send one JSON message; whether it went.
pub(super) async fn send<S, T>(socket: &mut WebSocketStream<S>, message: &T) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let Ok(text) = serde_json::to_string(message) else {
        return false;
    };
    socket.send(Message::text(text)).await.is_ok()
}
